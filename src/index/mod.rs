mod analyzer;
mod node;
mod search;
mod stem;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use tantivy::collector::{Count, TopDocs};
use tantivy::directory::MmapDirectory;
use tantivy::query::{BooleanQuery, Occur, Query, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions, Value,
};
use tantivy::tokenizer::TextAnalyzer;
use tantivy::{IndexReader, IndexWriter, ReloadPolicy, Searcher, TantivyDocument, Term};

pub use search::{Query as SearchQuery, SearchResult};

use crate::Result;
use crate::record::Record;
use analyzer::{ANALYZER, DssrTokenizer, PHRASE_ANALYZER};

#[derive(Clone, Copy)]
struct Fields {
    id: Field,
    source: Field,
    generation: Field,
    title: Field,
    title_phrase: Field,
    r#abstract: Field,
    creator: Field,
    contributor: Field,
    subject: Field,
    subject_facet: Field,
    r#type: Field,
    language: Field,
    is_part_of: Field,
    year: Field,
    issued: Field,
    record: Field,
}

struct Snapshot {
    searcher: Searcher,
    docs: f64,
    avg: HashMap<Field, f64>,
}

pub struct Index {
    fields: Fields,
    writer: Mutex<IndexWriter>,
    reader: IndexReader,
    snapshot: RwLock<Arc<Snapshot>>,
    state_path: PathBuf,
    state: Mutex<BTreeMap<String, String>>,
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let text = |analyzer: &str| {
        TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(analyzer)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        )
    };
    let keyword = STRING | tantivy::schema::FAST;
    let fields = Fields {
        id: b.add_text_field("id", keyword.clone()),
        source: b.add_text_field("source", STRING),
        generation: b.add_text_field("gen", STRING),
        title: b.add_text_field("title", text(ANALYZER)),
        title_phrase: b.add_text_field("title_phrase", text(PHRASE_ANALYZER)),
        r#abstract: b.add_text_field("abstract", text(ANALYZER)),
        creator: b.add_text_field("creator", text(ANALYZER)),
        contributor: b.add_text_field("contributor", text(ANALYZER)),
        subject: b.add_text_field("subject", text(ANALYZER)),
        subject_facet: b.add_text_field("subject_facet", keyword.clone()),
        r#type: b.add_text_field("type", keyword.clone()),
        language: b.add_text_field("language", keyword.clone()),
        is_part_of: b.add_text_field("isPartOf", keyword.clone()),
        year: b.add_text_field("year", keyword.clone()),
        issued: b.add_text_field("issued", keyword),
        record: b.add_text_field("record", STORED),
    };
    (b.build(), fields)
}

fn legacy_index(path: &Path) -> bool {
    path.join("index_meta.json").exists()
}

impl Index {
    pub fn open(path: &Path) -> Result<Index> {
        if legacy_index(path) {
            return Err(format!(
                "{} holds an index of an older dssr; run sudo dssr reindex",
                path.display()
            )
            .into());
        }
        let dir = path.join("tantivy");
        std::fs::create_dir_all(&dir)?;
        let (schema, fields) = schema();
        let index = tantivy::Index::open_or_create(MmapDirectory::open(&dir)?, schema)?;
        index.tokenizers().register(
            ANALYZER,
            TextAnalyzer::from(DssrTokenizer { gapless: false }),
        );
        index.tokenizers().register(
            PHRASE_ANALYZER,
            TextAnalyzer::from(DssrTokenizer { gapless: true }),
        );
        let writer = index.writer_with_num_threads(1, 50_000_000)?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let state_path = path.join("state.json");
        let state = match std::fs::read(&state_path) {
            Ok(b) => serde_json::from_slice(&b)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.into()),
        };
        let snapshot = RwLock::new(Arc::new(Self::snapshot(&reader, &fields)?));
        Ok(Index {
            fields,
            writer: Mutex::new(writer),
            reader,
            snapshot,
            state_path,
            state: Mutex::new(state),
        })
    }

    fn snapshot(reader: &IndexReader, f: &Fields) -> Result<Snapshot> {
        let searcher = reader.searcher();
        let docs = searcher.num_docs() as f64;
        let mut avg = HashMap::new();
        for field in [
            f.title,
            f.title_phrase,
            f.r#abstract,
            f.creator,
            f.contributor,
            f.subject,
        ] {
            let mut cardinality = 0u64;
            for seg in searcher.segment_readers() {
                let inverted = seg.inverted_index(field)?;
                let mut terms = inverted.terms().stream()?;
                while terms.advance() {
                    cardinality += terms.value().doc_freq as u64;
                }
            }
            avg.insert(
                field,
                if docs > 0.0 {
                    (cardinality as f64 / docs).ceil()
                } else {
                    0.0
                },
            );
        }
        Ok(Snapshot {
            searcher,
            docs,
            avg,
        })
    }

    fn refresh(&self) -> Result<()> {
        self.reader.reload()?;
        let snap = Self::snapshot(&self.reader, &self.fields)?;
        *self.snapshot.write().unwrap() = Arc::new(snap);
        Ok(())
    }

    fn current(&self) -> Arc<Snapshot> {
        self.snapshot.read().unwrap().clone()
    }

    fn to_doc(&self, r: &Record, generation: &str) -> Result<TantivyDocument> {
        let f = &self.fields;
        let mut d = TantivyDocument::new();
        d.add_text(f.id, &r.id);
        d.add_text(f.source, &r.source);
        d.add_text(f.generation, generation);
        d.add_text(f.record, serde_json::to_string(r)?);
        let mut add = |field: Field, values: &[String]| {
            for v in values {
                d.add_text(field, v);
            }
        };
        let one = |s: &str| {
            if s.is_empty() {
                vec![]
            } else {
                vec![s.to_string()]
            }
        };
        let names: Vec<String> = r.contributor.iter().map(|c| c.name.clone()).collect();
        add(f.title, &one(&r.title));
        add(f.title_phrase, &one(&r.title));
        add(f.r#abstract, &r.r#abstract);
        add(f.creator, &r.creator);
        add(f.contributor, &names);
        add(f.subject, &r.subject);
        add(f.subject_facet, &r.subject);
        add(f.r#type, &r.r#type);
        add(f.language, &r.language);
        add(f.is_part_of, &r.is_part_of);
        add(f.year, &one(r.year()));
        add(f.issued, &one(&r.issued));
        Ok(d)
    }

    pub fn apply(&self, upserts: &[Record], deletes: &[String], generation: &str) -> Result<()> {
        let mut w = self.writer.lock().unwrap();
        for r in upserts {
            w.delete_term(Term::from_field_text(self.fields.id, &r.id));
            w.add_document(self.to_doc(r, generation)?)?;
        }
        for id in deletes {
            w.delete_term(Term::from_field_text(self.fields.id, id));
        }
        w.commit()?;
        self.refresh()
    }

    pub fn get(&self, id: &str) -> Result<Option<Record>> {
        let snap = self.current();
        let q = TermQuery::new(
            Term::from_field_text(self.fields.id, id),
            IndexRecordOption::Basic,
        );
        let Some((_, addr)) = snap
            .searcher
            .search(&q, &TopDocs::with_limit(1).order_by_score())?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        let doc: TantivyDocument = snap.searcher.doc(addr)?;
        let Some(raw) = doc.get_first(self.fields.record).and_then(|v| v.as_str()) else {
            return Ok(None);
        };
        Ok(Some(serde_json::from_str(raw)?))
    }

    pub fn meta(&self, key: &str) -> String {
        self.state
            .lock()
            .unwrap()
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    pub fn set_meta(&self, key: &str, val: &str) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        let mut next = state.clone();
        if val.is_empty() {
            next.remove(key);
        } else {
            next.insert(key.to_string(), val.to_string());
        }
        let tmp = self.state_path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&next)?)?;
        std::fs::rename(&tmp, &self.state_path)?;
        *state = next;
        Ok(())
    }

    fn source_query(&self, source: &str) -> TermQuery {
        TermQuery::new(
            Term::from_field_text(self.fields.source, source),
            IndexRecordOption::Basic,
        )
    }

    pub fn count(&self, source: &str) -> Result<u64> {
        Ok(self
            .current()
            .searcher
            .search(&self.source_query(source), &Count)? as u64)
    }

    pub fn sweep(&self, source: &str, generation: &str) -> Result<usize> {
        let mut w = self.writer.lock().unwrap();
        let q = BooleanQuery::new(vec![
            (
                Occur::Must,
                Box::new(self.source_query(source)) as Box<dyn Query>,
            ),
            (
                Occur::MustNot,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.fields.generation, generation),
                    IndexRecordOption::Basic,
                )),
            ),
        ]);
        let n = self.current().searcher.search(&q, &Count)?;
        if n > 0 {
            w.delete_query(Box::new(q))?;
            w.commit()?;
            self.refresh()?;
        }
        Ok(n)
    }
}

#[cfg(test)]
pub mod tests;
