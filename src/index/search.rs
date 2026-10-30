use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock};

use levenshtein_automata::{DFA, Distance, LevenshteinAutomatonBuilder, SINK_STATE};
use serde::{Deserialize, Serialize};
use tantivy::columnar::StrColumn;
use tantivy::query::{BooleanQuery, Occur, PhraseQuery, TermQuery, TermSetQuery};
use tantivy::schema::{Field, IndexRecordOption, Value};
use tantivy::snippet::SnippetGenerator;
use tantivy::tokenizer::{TextAnalyzer, Token};
use tantivy::{DocAddress, TERMINATED, TantivyDocument, Term};

use super::analyzer::{DssrTokenizer, analyze};
use super::node::Node;
use super::{Index, Snapshot};
use crate::Result;
use crate::record::Record;

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub q: String,
    pub types: Vec<String>,
    pub year_from: String,
    pub year_to: String,
    pub collections: Vec<String>,
    pub languages: Vec<String>,
    pub newest: bool,
    pub page: usize,
    pub limit: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hit {
    #[serde(flatten)]
    pub record: Record,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub highlight: BTreeMap<String, Vec<String>>,
    pub score: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Facet {
    pub value: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub total: u64,
    pub page: usize,
    pub limit: usize,
    pub results: Vec<Hit>,
    pub facets: BTreeMap<String, Vec<Facet>>,
}

const FUZZY_WEIGHT: f64 = 0.2;

static LEVENSHTEIN: LazyLock<LevenshteinAutomatonBuilder> =
    LazyLock::new(|| LevenshteinAutomatonBuilder::new(1, true));

struct Automaton<'a>(&'a DFA);

impl tantivy_fst::Automaton for Automaton<'_> {
    type State = u32;

    fn start(&self) -> u32 {
        self.0.initial_state()
    }

    fn is_match(&self, state: &u32) -> bool {
        matches!(self.0.distance(*state), Distance::Exact(_))
    }

    fn can_match(&self, state: &u32) -> bool {
        *state != SINK_STATE
    }

    fn accept(&self, state: &u32, byte: u8) -> u32 {
        self.0.transition(*state, byte)
    }
}

fn split(s: &str) -> (Vec<&str>, Vec<&str>) {
    let (mut words, mut phrases) = (Vec::new(), Vec::new());
    for (i, part) in s.split('"').enumerate() {
        if i % 2 == 1 {
            phrases.push(part)
        } else {
            words.push(part)
        }
    }
    (words, phrases)
}

struct Builder<'a> {
    x: &'a Index,
    snap: &'a Snapshot,
}

impl Builder<'_> {
    fn boosts(&self) -> [(Field, f64); 5] {
        let f = &self.x.fields;
        [
            (f.title, 3.0),
            (f.creator, 2.0),
            (f.contributor, 2.0),
            (f.subject, 2.0),
            (f.r#abstract, 1.0),
        ]
    }

    fn term(&self, field: Field, text: &str, boost: f64) -> Result<Node> {
        let term = Term::from_field_text(field, text);
        let n = self.snap.searcher.doc_freq(&term)? as f64;
        let idf = (1.0 + (self.snap.docs - n + 0.5) / (n + 0.5)).ln();
        Ok(Node::Term {
            term,
            weight: boost * idf * idf,
            avg: self.snap.avg[&field],
        })
    }

    fn fuzzy(&self, field: Field, tok: &str, boost: f64) -> Result<Node> {
        let dfa = LEVENSHTEIN.build_dfa(tok);
        let mut candidates = BTreeMap::new();
        for seg in self.snap.searcher.segment_readers() {
            let inverted = seg.inverted_index(field)?;
            let mut stream = inverted.terms().search(Automaton(&dfa)).into_stream()?;
            while stream.advance() {
                if let (Ok(t), Distance::Exact(d)) =
                    (std::str::from_utf8(stream.key()), dfa.eval(stream.key()))
                {
                    candidates.insert(t.to_string(), d);
                }
            }
        }
        if candidates.is_empty() || (candidates.len() == 1 && candidates.contains_key(tok)) {
            return self.term(field, tok, boost);
        }
        let children = candidates
            .iter()
            .map(|(t, d)| self.term(field, t, boost / (*d as f64 + 1.0)))
            .collect::<Result<_>>()?;
        Ok(Node::Any { children, min: 0 })
    }

    fn term_clause(&self, tok: &str) -> Result<Node> {
        let mut children = Vec::new();
        for (field, boost) in self.boosts() {
            children.push(self.term(field, tok, boost)?);
            if tok.chars().count() >= 5 {
                children.push(self.fuzzy(field, tok, boost * FUZZY_WEIGHT)?);
            }
        }
        Ok(Node::Any { children, min: 0 })
    }

    fn phrase(&self, field: Field, tokens: &[Token], boost: f64) -> Result<Node> {
        if tokens.len() == 1 {
            return self.term(field, &tokens[0].text, boost);
        }
        let first = tokens[0].position;
        let terms = tokens
            .iter()
            .map(|t| (t.position - first, Term::from_field_text(field, &t.text)))
            .collect();
        let mut must = vec![Node::Filter(Arc::new(PhraseQuery::new_with_offset(terms)))];
        for t in tokens {
            must.push(self.term(field, &t.text, boost)?);
        }
        Ok(Node::All {
            must,
            should: vec![],
        })
    }

    fn phrase_clause(&self, tokens: &[Token]) -> Result<Node> {
        let children = self
            .boosts()
            .into_iter()
            .map(|(field, boost)| self.phrase(field, tokens, boost))
            .collect::<Result<_>>()?;
        Ok(Node::Any { children, min: 0 })
    }

    fn filter(&self, q: &Query) -> Option<Node> {
        let f = &self.x.fields;
        let mut clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = Vec::new();
        for (field, values) in [
            (f.r#type, &q.types),
            (f.is_part_of, &q.collections),
            (f.language, &q.languages),
        ] {
            if !values.is_empty() {
                let any = values
                    .iter()
                    .map(|v| {
                        (
                            Occur::Should,
                            Box::new(TermQuery::new(
                                Term::from_field_text(field, v),
                                IndexRecordOption::Basic,
                            )) as Box<dyn tantivy::query::Query>,
                        )
                    })
                    .collect();
                clauses.push((Occur::Must, Box::new(BooleanQuery::new(any))));
            }
        }
        if !q.year_from.is_empty() {
            let (from, to) = (
                q.year_from.parse::<u32>().unwrap_or(0),
                q.year_to.parse::<u32>().unwrap_or(0),
            );
            let years = (from..=to).map(|y| Term::from_field_text(f.year, &format!("{y:04}")));
            clauses.push((Occur::Must, Box::new(TermSetQuery::new(years))));
        }
        if clauses.is_empty() {
            return None;
        }
        Some(Node::Filter(Arc::new(BooleanQuery::new(clauses))))
    }
}

type Ranked = (f64, String, Option<String>, DocAddress);

fn names(
    col: &StrColumn,
    ords: impl Iterator<Item = u64>,
) -> std::io::Result<(Vec<u64>, Vec<String>)> {
    let mut ords: Vec<u64> = ords.collect();
    ords.sort_unstable();
    ords.dedup();
    let mut out = Vec::with_capacity(ords.len());
    col.dictionary()
        .sorted_ords_to_term_cb(&ords, |t| out.push(String::from_utf8_lossy(t).into_owned()))?;
    Ok((ords, out))
}

fn lookup(names: &(Vec<u64>, Vec<String>), ord: u64) -> String {
    names
        .0
        .binary_search(&ord)
        .ok()
        .and_then(|i| names.1.get(i))
        .cloned()
        .unwrap_or_default()
}

struct Found {
    total: u64,
    hits: Vec<(f64, DocAddress)>,
    facets: BTreeMap<String, Vec<Facet>>,
}

impl Index {
    fn facet_fields(&self) -> [(&'static str, &'static str); 5] {
        [
            ("type", "type"),
            ("year", "year"),
            ("isPartOf", "isPartOf"),
            ("language", "language"),
            ("subject", "subject_facet"),
        ]
    }

    fn collect(&self, snap: &Snapshot, root: &Node, newest: bool) -> Result<Found> {
        let mut total = 0;
        let mut hits: Vec<Ranked> = Vec::new();
        let mut counts: HashMap<&str, HashMap<String, usize>> = HashMap::new();

        for (ord, seg) in snap.searcher.segment_readers().iter().enumerate() {
            let mut refs = Vec::new();
            let mut scorer = root.scorer(&snap.searcher, seg)?;
            let fast = seg.fast_fields();
            let ids = fast.str("id")?.ok_or("id column")?;
            let issued = if newest { fast.str("issued")? } else { None };
            let mut facets = Vec::new();
            for (name, field) in self.facet_fields() {
                if let Some(col) = fast.str(field)? {
                    let n = col.num_terms();
                    facets.push((name, col, vec![0usize; n], vec![TERMINATED; n]));
                }
            }
            let alive = seg.alive_bitset();
            while scorer.doc() != TERMINATED {
                let doc = scorer.doc();
                if alive.is_none_or(|a| a.is_alive(doc)) {
                    total += 1;
                    let id = ids.term_ords(doc).next();
                    let date = issued.as_ref().and_then(|c| c.term_ords(doc).next());
                    refs.push((scorer.score(), doc, id, date));
                    for (_, col, count, last) in &mut facets {
                        for o in col.term_ords(doc) {
                            if last[o as usize] != doc {
                                last[o as usize] = doc;
                                count[o as usize] += 1;
                            }
                        }
                    }
                }
                scorer.advance();
            }
            for (name, col, count, _) in facets {
                let merged = counts.entry(name).or_default();
                let (ords, ns): (Vec<u64>, Vec<usize>) = count
                    .into_iter()
                    .enumerate()
                    .filter(|(_, n)| *n > 0)
                    .map(|(o, n)| (o as u64, n))
                    .unzip();
                let mut ns = ns.into_iter();
                col.dictionary().sorted_ords_to_term_cb(&ords, |term| {
                    *merged
                        .entry(String::from_utf8_lossy(term).into_owned())
                        .or_default() += ns.next().unwrap_or(0);
                })?;
            }
            let id_names = names(&ids, refs.iter().filter_map(|r| r.2))?;
            let date_names = match &issued {
                Some(col) => names(col, refs.iter().filter_map(|r| r.3))?,
                None => (vec![], vec![]),
            };
            for (score, doc, id, date) in refs {
                let id = id.map(|o| lookup(&id_names, o)).unwrap_or_default();
                hits.push((
                    score,
                    id,
                    date.map(|o| lookup(&date_names, o)),
                    DocAddress::new(ord as u32, doc),
                ));
            }
        }
        hits.sort_by(|a, b| {
            let date = match (&a.2, &b.2) {
                (Some(x), Some(y)) => y.cmp(x),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            };
            date.then_with(|| b.0.total_cmp(&a.0))
                .then_with(|| a.1.cmp(&b.1))
        });
        let hits = hits
            .into_iter()
            .map(|(score, _, _, addr)| (score, addr))
            .collect();
        let mut facets = BTreeMap::new();
        for (name, _) in self.facet_fields() {
            let mut fs: Vec<Facet> = counts
                .remove(name)
                .unwrap_or_default()
                .into_iter()
                .map(|(value, count)| Facet { value, count })
                .collect();
            fs.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.value.cmp(&b.value)));
            fs.truncate(10);
            facets.insert(name.to_string(), fs);
        }
        Ok(Found {
            total,
            hits,
            facets,
        })
    }

    pub fn search(&self, q: &Query) -> Result<SearchResult> {
        let mut out = SearchResult {
            total: 0,
            page: q.page,
            limit: q.limit,
            results: vec![],
            facets: BTreeMap::new(),
        };
        let snap = self.current();
        let b = Builder {
            x: self,
            snap: &snap,
        };
        let (words, phrases) = split(&q.q);
        let joined = words.join(" ");
        let tokens = analyze(&joined, false);

        let mut clauses = Vec::new();
        for t in &tokens {
            clauses.push(b.term_clause(&t.text)?);
        }
        for p in phrases {
            let pt = analyze(p, false);
            if !pt.is_empty() {
                clauses.push(b.phrase_clause(&pt)?);
            }
        }
        if clauses.is_empty() {
            return Ok(out);
        }
        let mut should = Vec::new();
        if tokens.len() > 1 {
            should.push(b.phrase(self.fields.title_phrase, &analyze(&joined, true), 3.0)?);
        }
        let filter = b.filter(q);

        let n = tokens.len();
        let mut must = clauses.clone();
        if n >= 4 {
            let rest = must.split_off(n);
            must = std::iter::once(Node::Any {
                children: must,
                min: n * 3 / 4,
            })
            .chain(rest)
            .collect();
        }
        must.extend(filter.clone());
        let mut root = Node::All {
            must,
            should: should.clone(),
        };
        let mut found = self.collect(&snap, &root, q.newest)?;
        if found.total == 0 && clauses.len() > 1 {
            let any = Node::Any {
                children: clauses.into_iter().chain(should).collect(),
                min: 1,
            };
            root = Node::All {
                must: std::iter::once(any).chain(filter).collect(),
                should: vec![],
            };
            found = self.collect(&snap, &root, q.newest)?;
        }

        out.total = found.total;
        out.facets = found.facets;
        let highlighters = self.highlighters(&snap, &root)?;
        for (score, addr) in found
            .hits
            .into_iter()
            .skip((q.page - 1) * q.limit)
            .take(q.limit)
        {
            let doc: TantivyDocument = snap.searcher.doc(addr)?;
            let raw = doc
                .get_first(self.fields.record)
                .and_then(|v| v.as_str())
                .unwrap_or("{}");
            let record: Record = serde_json::from_str(raw)?;
            let mut highlight = BTreeMap::new();
            for (name, generator) in &highlighters {
                let values = if *name == "title" {
                    std::slice::from_ref(&record.title)
                } else {
                    &record.r#abstract[..]
                };
                let best = values
                    .iter()
                    .map(|v| generator.snippet(v))
                    .filter(|s| !s.highlighted().is_empty())
                    .max_by_key(|s| s.highlighted().len());
                if let Some(mut s) = best {
                    s.set_snippet_prefix_postfix("<mark>", "</mark>");
                    highlight.insert(name.to_string(), vec![s.to_html()]);
                }
            }
            out.results.push(Hit {
                record,
                highlight,
                score,
            });
        }
        Ok(out)
    }

    fn highlighters(
        &self,
        snap: &Snapshot,
        root: &Node,
    ) -> Result<Vec<(&'static str, SnippetGenerator)>> {
        let mut out = Vec::new();
        for (name, field) in [
            ("title", self.fields.title),
            ("abstract", self.fields.r#abstract),
        ] {
            let mut terms = BTreeMap::new();
            let mut found = Vec::new();
            root.visit_terms(&mut |t| {
                if t.field() == field {
                    found.push(t.clone());
                }
            });
            for t in found {
                let df = snap.searcher.doc_freq(&t)?;
                if let (true, Some(text)) = (df > 0, t.value().as_str()) {
                    terms.insert(text.to_string(), 1.0 / (1.0 + df as f32));
                }
            }
            let mut g = SnippetGenerator::new(
                terms,
                TextAnalyzer::from(DssrTokenizer { gapless: false }),
                field,
                200,
            );
            g.set_max_num_chars(200);
            out.push((name, g));
        }
        Ok(out)
    }
}
