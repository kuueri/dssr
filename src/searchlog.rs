use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::Serialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::config;
use crate::index::{SearchQuery, SearchResult};
use crate::log;

const BUF_BYTES: usize = 64 * 1024;
const CHANNEL_EVENTS: usize = 4096;

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub time: String,
    pub level: &'static str,
    pub msg: &'static str,
    pub client: String,
    pub q: String,
    pub filter: Filter,
    pub sort: &'static str,
    pub page: usize,
    pub limit: usize,
    pub total: u64,
    pub took_ms: u64,
    pub hits: Vec<EventHit>,
    #[serde(skip_serializing_if = "is_zero")]
    pub dropped: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Filter {
    #[serde(rename = "type", skip_serializing_if = "Vec::is_empty")]
    pub types: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub collection: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub language: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub source: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub year: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EventHit {
    pub id: String,
    pub score: f64,
    pub title: String,
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

pub fn event(q: &SearchQuery, res: &SearchResult, client: &str, took: Duration, n: usize) -> Event {
    let year = if q.year_from.is_empty() {
        String::new()
    } else if q.year_from == q.year_to {
        q.year_from.clone()
    } else {
        format!("{}-{}", q.year_from, q.year_to)
    };
    Event {
        time: chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        level: "INFO",
        msg: "search",
        client: client.to_string(),
        q: q.q.clone(),
        filter: Filter {
            types: q.types.clone(),
            collection: q.collections.clone(),
            language: q.languages.clone(),
            source: q.sources.clone(),
            year,
        },
        sort: if q.newest { "newest" } else { "relevance" },
        page: q.page,
        limit: q.limit,
        total: res.total,
        took_ms: took.as_millis() as u64,
        hits: res
            .results
            .iter()
            .take(n)
            .map(|h| EventHit {
                id: h.record.id.clone(),
                score: h.score,
                title: h.record.title.clone(),
            })
            .collect(),
        dropped: 0,
    }
}

pub struct SearchLog {
    tx: mpsc::Sender<Event>,
    pub hits: usize,
    dropped: AtomicU64,
}

impl SearchLog {
    pub fn record(&self, mut e: Event) {
        e.dropped = self.dropped.swap(0, Ordering::Relaxed);
        if let Err(err) = self.tx.try_send(e) {
            let e = match err {
                mpsc::error::TrySendError::Full(e) | mpsc::error::TrySendError::Closed(e) => e,
            };
            self.dropped
                .fetch_add(e.dropped.saturating_add(1), Ordering::Relaxed);
        }
    }
}

pub fn start(cfg: &config::SearchLog) -> crate::Result<(Arc<SearchLog>, JoinHandle<()>)> {
    let max_bytes = cfg.max_size_mb.saturating_mul(1024 * 1024);
    if cfg.output == "stdout" {
        return Ok(spawn(Sink::Stdout, CHANNEL_EVENTS, max_bytes, cfg.hits));
    }
    let path = PathBuf::from(&cfg.output);
    let file =
        open_append(&path).map_err(|e| format!("search_log: open {}: {e}", path.display()))?;
    Ok(spawn(
        Sink::File { path, file },
        CHANNEL_EVENTS,
        max_bytes,
        cfg.hits,
    ))
}

fn spawn(
    sink: Sink,
    capacity: usize,
    max_bytes: u64,
    hits: usize,
) -> (Arc<SearchLog>, JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(capacity);
    let log = Arc::new(SearchLog {
        tx,
        hits,
        dropped: AtomicU64::new(0),
    });
    let task = tokio::task::spawn_blocking(move || write(rx, sink, max_bytes));
    (log, task)
}

enum Sink {
    Stdout,
    File { path: PathBuf, file: File },
}

struct Output {
    inner: BufWriter<Box<dyn Write>>,
    path: Option<PathBuf>,
    written: u64,
}

impl Output {
    fn new(sink: Sink) -> Output {
        match sink {
            Sink::Stdout => Output {
                inner: BufWriter::with_capacity(BUF_BYTES, Box::new(std::io::stdout())),
                path: None,
                written: 0,
            },
            Sink::File { path, file } => {
                let written = file.metadata().map(|m| m.len()).unwrap_or(0);
                Output {
                    inner: BufWriter::with_capacity(BUF_BYTES, Box::new(file)),
                    path: Some(path),
                    written,
                }
            }
        }
    }

    fn write_line(&mut self, line: &str, max_bytes: u64) -> std::io::Result<()> {
        let n = line.len() as u64;
        if self.path.is_some() && self.written.saturating_add(n) > max_bytes {
            self.rotate()?;
        }
        self.inner.write_all(line.as_bytes())?;
        self.written = self.written.saturating_add(n);
        Ok(())
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        self.inner.flush()?;
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        let next = next_rotation(&path)?;
        std::fs::rename(&path, next)?;
        let file = open_append(&path)?;
        self.inner = BufWriter::with_capacity(BUF_BYTES, Box::new(file));
        self.written = 0;
        Ok(())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn next_rotation(path: &Path) -> std::io::Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| std::io::Error::other("search log path has no file name"))?;
    let prefix = format!("{name}.");
    let mut max = 0usize;
    for entry in std::fs::read_dir(parent)? {
        let entry = entry?;
        let Some(file) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let Some(n) = file
            .strip_prefix(&prefix)
            .and_then(|s| s.parse::<usize>().ok())
        else {
            continue;
        };
        max = max.max(n);
    }
    let next = max
        .checked_add(1)
        .ok_or_else(|| std::io::Error::other("search log rotation number overflow"))?;
    let mut rotated = path.as_os_str().to_owned();
    rotated.push(format!(".{next:02}"));
    Ok(PathBuf::from(rotated))
}

fn open_append(path: &Path) -> std::io::Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o640)
        .open(path)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o640))?;
    Ok(file)
}

fn write_event(out: &mut Output, e: &Event, max_bytes: u64) -> std::io::Result<()> {
    let mut line = serde_json::to_string(e).map_err(std::io::Error::other)?;
    line.push('\n');
    out.write_line(&line, max_bytes)
}

fn write(mut rx: mpsc::Receiver<Event>, sink: Sink, max_bytes: u64) {
    let mut out = Output::new(sink);
    loop {
        let Some(first) = rx.blocking_recv() else {
            if let Err(e) = out.flush() {
                log::error("search log write failed", &[("err", json!(e.to_string()))]);
            }
            return;
        };
        let mut error = None;
        let mut current = Some(first);
        while let Some(e) = current {
            if let Err(e) = write_event(&mut out, &e, max_bytes) {
                error = Some(e.to_string());
            }
            current = rx.try_recv().ok();
        }
        if let Err(e) = out.flush()
            && error.is_none()
        {
            error = Some(e.to_string());
        }
        if let Some(e) = error {
            log::error("search log write failed", &[("err", json!(e))]);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::Duration;

    use tokio::sync::mpsc;

    use super::*;
    use crate::index::Hit;
    use crate::index::tests::temp_dir;
    use crate::record::Record;

    fn result(ids: &[&str]) -> SearchResult {
        SearchResult {
            total: 42,
            page: 1,
            limit: 10,
            results: ids
                .iter()
                .enumerate()
                .map(|(i, id)| Hit {
                    record: Record {
                        id: id.to_string(),
                        title: format!("T{i}"),
                        ..Default::default()
                    },
                    highlight: Default::default(),
                    score: 10.0 - i as f64,
                })
                .collect(),
            facets: Default::default(),
        }
    }

    fn sample() -> Event {
        let q = SearchQuery {
            q: "wisata".into(),
            page: 1,
            limit: 10,
            ..Default::default()
        };
        event(&q, &result(&["repo:1/1"]), "test", Duration::ZERO, 10)
    }

    fn lines(p: &std::path::Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(p)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    #[test]
    fn event_samples_hits() {
        let q = SearchQuery {
            q: "wisata ziarah".into(),
            types: vec!["Thesis".into()],
            year_from: "2020".into(),
            year_to: "2024".into(),
            page: 1,
            limit: 10,
            ..Default::default()
        };
        let e = event(
            &q,
            &result(&["repo:1/1", "slims:7", "repo:1/2"]),
            "digilib",
            Duration::from_millis(12),
            2,
        );
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(
            (
                v["msg"].as_str(),
                v["client"].as_str(),
                v["total"].as_u64(),
                v["took_ms"].as_u64()
            ),
            (Some("search"), Some("digilib"), Some(42), Some(12))
        );
        assert_eq!(
            v["hits"]
                .as_array()
                .unwrap()
                .iter()
                .map(|h| h["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["repo:1/1", "slims:7"]
        );
        assert_eq!(
            v["filter"],
            serde_json::json!({"type": ["Thesis"], "year": "2020-2024"})
        );
        assert!(v.get("dropped").is_none());
        let f: crate::EvalFilter = serde_json::from_value(v["filter"].clone()).unwrap();
        assert_eq!(
            (f.r#type, f.year.as_str()),
            (vec!["Thesis".to_string()], "2020-2024")
        );
    }

    #[test]
    fn event_is_one_line() {
        let q = SearchQuery {
            q: "a \"b\" \\ c\nd ä".into(),
            page: 1,
            limit: 10,
            ..Default::default()
        };
        let line =
            serde_json::to_string(&event(&q, &result(&[]), "x", Duration::ZERO, 10)).unwrap();
        assert!(!line.contains('\n'));
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["q"], "a \"b\" \\ c\nd ä");
        assert!(v.get("filter").unwrap().as_object().unwrap().is_empty());
    }

    #[tokio::test]
    async fn writer_flushes_on_close() {
        let (_t, dir) = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("search.log");
        let cfg = crate::config::SearchLog {
            output: path.to_str().unwrap().into(),
            hits: 10,
            max_size_mb: 64,
        };
        let (log, task) = start(&cfg).unwrap();
        for _ in 0..3 {
            log.record(sample());
        }
        drop(log);
        task.await.unwrap();
        assert_eq!(lines(&path).len(), 3);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }

    #[tokio::test]
    async fn writer_rotates() {
        let (_t, dir) = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("search.log");
        let line = serde_json::to_string(&sample()).unwrap().len() as u64 + 1;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        let (log, task) = spawn(
            Sink::File {
                path: path.clone(),
                file,
            },
            64,
            line * 3,
            10,
        );
        for _ in 0..10 {
            log.record(sample());
        }
        drop(log);
        task.await.unwrap();
        let (now, first, second, third) = (
            lines(&path),
            lines(&dir.join("search.log.01")),
            lines(&dir.join("search.log.02")),
            lines(&dir.join("search.log.03")),
        );
        assert_eq!(
            (now.len(), first.len(), second.len(), third.len()),
            (1, 3, 3, 3)
        );
        assert!(std::fs::metadata(&path).unwrap().len() <= line * 3);
    }

    #[tokio::test]
    async fn full_channel_drops_and_counts() {
        let (_t, dir) = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("search.log");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        let (tx, rx) = mpsc::channel(1);
        let log = SearchLog {
            tx,
            hits: 10,
            dropped: AtomicU64::new(0),
        };
        log.record(sample());
        log.record(sample());
        log.record(sample());
        assert_eq!(log.dropped.load(Ordering::Relaxed), 2);
        let sink = Sink::File {
            path: path.clone(),
            file,
        };
        let task = tokio::task::spawn_blocking(move || write(rx, sink, u64::MAX));
        while log.tx.capacity() == 0 {
            tokio::task::yield_now().await;
        }
        log.record(sample());
        drop(log);
        task.await.unwrap();
        let l = lines(&path);
        assert_eq!(l.len(), 2);
        assert_eq!(l[1]["dropped"], 2);
    }

    #[test]
    fn start_fails_on_bad_path() {
        let (_t, dir) = temp_dir();
        let path = dir.join("missing").join("search.log");
        let cfg = crate::config::SearchLog {
            output: path.to_str().unwrap().into(),
            hits: 10,
            max_size_mb: 64,
        };
        let err = start(&cfg).err().unwrap().to_string();
        assert!(err.contains(path.to_str().unwrap()), "{err}");
    }
}
