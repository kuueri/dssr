use std::sync::Arc;

use tantivy::fieldnorm::FieldNormReader;
use tantivy::postings::{Postings, SegmentPostings};
use tantivy::query::{EnableScoring, Query, Scorer};
use tantivy::schema::IndexRecordOption;
use tantivy::{DocId, DocSet, Searcher, SegmentReader, TERMINATED, Term};

const K1: f64 = 1.2;
const B: f64 = 0.75;

#[derive(Clone, Debug)]
pub enum Node {
    Term { term: Term, weight: f64, avg: f64 },
    Any { children: Vec<Node>, min: usize },
    All { must: Vec<Node>, should: Vec<Node> },
    Filter(Arc<dyn Query>),
}

pub trait NodeScorer: Send {
    fn doc(&self) -> DocId;
    fn advance(&mut self) -> DocId;
    fn seek(&mut self, target: DocId) -> DocId;
    fn score(&mut self) -> f64;
}

impl Node {
    pub fn visit_terms<'a>(&'a self, f: &mut dyn FnMut(&'a Term)) {
        match self {
            Node::Term { term, .. } => f(term),
            Node::Any { children, .. } => children.iter().for_each(|c| c.visit_terms(f)),
            Node::All { must, should } => must.iter().chain(should).for_each(|c| c.visit_terms(f)),
            Node::Filter(_) => {}
        }
    }

    pub fn scorer(
        &self,
        searcher: &Searcher,
        reader: &SegmentReader,
    ) -> tantivy::Result<Box<dyn NodeScorer>> {
        Ok(match self {
            Node::Term { term, weight, avg } => {
                let postings = reader
                    .inverted_index(term.field())?
                    .read_postings(term, IndexRecordOption::WithFreqs)?;
                match postings {
                    Some(postings) => Box::new(TermScorer {
                        postings,
                        norms: reader.get_fieldnorms_reader(term.field())?,
                        weight: *weight,
                        avg: avg.max(1.0),
                    }),
                    None => Box::new(Empty),
                }
            }
            Node::Any { children, min } => {
                let children = children
                    .iter()
                    .map(|c| c.scorer(searcher, reader))
                    .collect::<tantivy::Result<Vec<_>>>()?;
                let mut s = AnyScorer {
                    total: children.len(),
                    children,
                    min: (*min).max(1),
                    doc: 0,
                };
                s.settle();
                Box::new(s)
            }
            Node::All { must, should } => {
                let must = must
                    .iter()
                    .map(|c| c.scorer(searcher, reader))
                    .collect::<tantivy::Result<Vec<_>>>()?;
                let should = should
                    .iter()
                    .map(|c| c.scorer(searcher, reader))
                    .collect::<tantivy::Result<Vec<_>>>()?;
                let start = must.iter().map(|m| m.doc()).max().unwrap_or(TERMINATED);
                let mut s = AllScorer {
                    must,
                    should,
                    doc: 0,
                };
                s.align(start);
                Box::new(s)
            }
            Node::Filter(q) => {
                let w = q.weight(EnableScoring::disabled_from_searcher(searcher))?;
                Box::new(FilterScorer(w.scorer(reader, 1.0)?))
            }
        })
    }
}

struct Empty;

impl NodeScorer for Empty {
    fn doc(&self) -> DocId {
        TERMINATED
    }
    fn advance(&mut self) -> DocId {
        TERMINATED
    }
    fn seek(&mut self, _: DocId) -> DocId {
        TERMINATED
    }
    fn score(&mut self) -> f64 {
        0.0
    }
}

struct TermScorer {
    postings: SegmentPostings,
    norms: FieldNormReader,
    weight: f64,
    avg: f64,
}

impl NodeScorer for TermScorer {
    fn doc(&self) -> DocId {
        self.postings.doc()
    }
    fn advance(&mut self) -> DocId {
        self.postings.advance()
    }
    fn seek(&mut self, target: DocId) -> DocId {
        if self.postings.doc() >= target {
            self.postings.doc()
        } else {
            self.postings.seek(target)
        }
    }
    fn score(&mut self) -> f64 {
        let tf = (self.postings.term_freq() as f64).sqrt();
        let len = self.norms.fieldnorm(self.postings.doc()) as f64;
        self.weight * tf * K1 / (tf + K1 * (1.0 - B + B * len / self.avg))
    }
}

struct FilterScorer(Box<dyn Scorer>);

impl NodeScorer for FilterScorer {
    fn doc(&self) -> DocId {
        self.0.doc()
    }
    fn advance(&mut self) -> DocId {
        self.0.advance()
    }
    fn seek(&mut self, target: DocId) -> DocId {
        if self.0.doc() >= target {
            self.0.doc()
        } else {
            self.0.seek(target)
        }
    }
    fn score(&mut self) -> f64 {
        0.0
    }
}

struct AnyScorer {
    children: Vec<Box<dyn NodeScorer>>,
    total: usize,
    min: usize,
    doc: DocId,
}

impl AnyScorer {
    fn settle(&mut self) -> DocId {
        loop {
            let doc = self
                .children
                .iter()
                .map(|c| c.doc())
                .min()
                .unwrap_or(TERMINATED);
            self.doc = doc;
            if doc == TERMINATED
                || self.children.iter().filter(|c| c.doc() == doc).count() >= self.min
            {
                return doc;
            }
            for c in self.children.iter_mut().filter(|c| c.doc() == doc) {
                c.advance();
            }
        }
    }
}

impl NodeScorer for AnyScorer {
    fn doc(&self) -> DocId {
        self.doc
    }
    fn advance(&mut self) -> DocId {
        let doc = self.doc;
        for c in self.children.iter_mut().filter(|c| c.doc() == doc) {
            c.advance();
        }
        self.settle()
    }
    fn seek(&mut self, target: DocId) -> DocId {
        if self.doc >= target {
            return self.doc;
        }
        for c in &mut self.children {
            c.seek(target);
        }
        self.settle()
    }
    fn score(&mut self) -> f64 {
        let doc = self.doc;
        let (mut sum, mut matched) = (0.0, 0);
        for c in self.children.iter_mut().filter(|c| c.doc() == doc) {
            sum += c.score();
            matched += 1;
        }
        sum * matched as f64 / self.total as f64
    }
}

struct AllScorer {
    must: Vec<Box<dyn NodeScorer>>,
    should: Vec<Box<dyn NodeScorer>>,
    doc: DocId,
}

impl AllScorer {
    fn align(&mut self, mut target: DocId) -> DocId {
        'outer: loop {
            if target == TERMINATED || self.must.is_empty() {
                self.doc = TERMINATED;
                return TERMINATED;
            }
            for m in &mut self.must {
                let d = m.seek(target);
                if d > target {
                    target = d;
                    continue 'outer;
                }
            }
            self.doc = target;
            return target;
        }
    }
}

impl NodeScorer for AllScorer {
    fn doc(&self) -> DocId {
        self.doc
    }
    fn advance(&mut self) -> DocId {
        let next = self.must[0].advance();
        self.align(next)
    }
    fn seek(&mut self, target: DocId) -> DocId {
        if self.doc >= target {
            self.doc
        } else {
            self.align(target)
        }
    }
    fn score(&mut self) -> f64 {
        let doc = self.doc;
        let mut sum: f64 = self.must.iter_mut().map(|m| m.score()).sum();
        for s in &mut self.should {
            if s.seek(doc) == doc {
                sum += s.score();
            }
        }
        sum
    }
}
