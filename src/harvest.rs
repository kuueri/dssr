use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, FixedOffset, Local, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::config::{Source, schedule};
use crate::index::Index;
use crate::{log, oai, record};

#[derive(Debug, PartialEq)]
pub enum Error {
    Busy,
    UnknownSource,
    Suspended,
    Cancelled,
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(match self {
            Error::Busy => "a harvest is already running for this source",
            Error::UnknownSource => "unknown source",
            Error::Suspended => "the schedule of this source is suspended",
            Error::Cancelled => "harvest cancelled",
            Error::Other(e) => e,
        })
    }
}

impl From<String> for Error {
    fn from(e: String) -> Error {
        Error::Other(e)
    }
}

impl From<crate::Error> for Error {
    fn from(e: crate::Error) -> Error {
        Error::Other(e.to_string())
    }
}

impl From<tokio::task::JoinError> for Error {
    fn from(e: tokio::task::JoinError) -> Error {
        Error::Other(e.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Run {
    pub full: bool,
    pub started: DateTime<FixedOffset>,
    pub finished: DateTime<FixedOffset>,
    pub upserted: usize,
    pub deleted: usize,
    pub swept: usize,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceStatus {
    pub id: String,
    pub name: String,
    pub docs: u64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cursor: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub suspended: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<DateTime<FixedOffset>>,
    pub running: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<Run>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub dropped: BTreeMap<String, usize>,
}

#[derive(Default)]
struct State {
    running: bool,
    cancel: Option<watch::Sender<bool>>,
    last: Option<Run>,
    dropped: BTreeMap<String, usize>,
}

struct Inner {
    idx: Arc<Index>,
    sources: Vec<Source>,
    agent: String,
    backoff: Duration,
    location: Mutex<Option<chrono_tz::Tz>>,
    state: Mutex<HashMap<String, State>>,
    shutdown: watch::Sender<bool>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

#[derive(Clone)]
pub struct Harvester(Arc<Inner>);

fn now() -> DateTime<FixedOffset> {
    Local::now().fixed_offset()
}

async fn cancelled(mut own: watch::Receiver<bool>, mut shutdown: watch::Receiver<bool>) {
    tokio::select! {
        _ = own.wait_for(|v| *v) => {}
        _ = shutdown.wait_for(|v| *v) => {}
    }
}

fn local_id(identifier: &str) -> &str {
    identifier.rsplit(':').next().unwrap_or(identifier)
}

fn set_names(specs: &[String], sets: &HashMap<String, String>) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for s in specs {
        if let Some(n) = sets.get(s)
            && !n.is_empty()
            && !names.contains(n)
        {
            names.push(n.clone());
        }
    }
    names
}

impl Harvester {
    pub fn new(idx: Arc<Index>, sources: Vec<Source>, agent: &str) -> Harvester {
        Self::with_backoff(idx, sources, agent, Duration::from_secs(2))
    }

    fn with_backoff(
        idx: Arc<Index>,
        sources: Vec<Source>,
        agent: &str,
        backoff: Duration,
    ) -> Harvester {
        let state = sources
            .iter()
            .map(|s| (s.id.clone(), State::default()))
            .collect();
        Harvester(Arc::new(Inner {
            idx,
            sources,
            agent: agent.to_string(),
            backoff,
            location: Mutex::new(None),
            state: Mutex::new(state),
            shutdown: watch::channel(false).0,
            tasks: Mutex::new(Vec::new()),
        }))
    }

    pub fn schedule(&self, loc: chrono_tz::Tz) -> Result<(), Error> {
        *self.0.location.lock().unwrap() = Some(loc);
        for s in &self.0.sources {
            let cron = schedule(&s.schedule).map_err(|e| format!("source {}: {e}", s.id))?;
            let (h, id) = (self.clone(), s.id.clone());
            let mut shutdown = self.0.shutdown.subscribe();
            let task = tokio::spawn(async move {
                loop {
                    let now = Utc::now().with_timezone(&loc);
                    let Ok(next) = cron.find_next_occurrence(&now, false) else {
                        return;
                    };
                    let wait = (next.with_timezone(&Utc) - Utc::now())
                        .to_std()
                        .unwrap_or_default();
                    tokio::select! {
                        _ = tokio::time::sleep(wait) => {}
                        _ = shutdown.wait_for(|v| *v) => return,
                    }
                    if let Err(e) = h.start_scheduled(&id) {
                        log::warn(
                            "scheduled harvest not started",
                            &[("source", json!(id)), ("err", json!(e.to_string()))],
                        );
                    }
                }
            });
            self.0.tasks.lock().unwrap().push(task);
        }
        Ok(())
    }

    pub async fn stop(&self) {
        self.0.shutdown.send_replace(true);
        let tasks: Vec<_> = self.0.tasks.lock().unwrap().drain(..).collect();
        for t in tasks {
            let _ = t.await;
        }
    }

    pub fn start(&self, id: &str, full: bool) -> Result<(), Error> {
        let rx = self.begin(id)?;
        let (h, id) = (self.clone(), id.to_string());
        let task = tokio::spawn(async move {
            let _ = h.run_started(&id, full, rx).await;
        });
        self.0.tasks.lock().unwrap().push(task);
        Ok(())
    }

    pub fn start_scheduled(&self, id: &str) -> Result<(), Error> {
        if !self.0.idx.meta(&format!("suspended:{id}")).is_empty() {
            return Err(Error::Suspended);
        }
        self.start(id, false)
    }

    #[cfg(test)]
    pub async fn run(&self, id: &str, full: bool) -> Result<Run, Error> {
        let rx = self.begin(id)?;
        self.run_started(id, full, rx).await
    }

    pub fn cancel(&self, id: &str) -> Result<bool, Error> {
        let state = self.0.state.lock().unwrap();
        let st = state.get(id).ok_or(Error::UnknownSource)?;
        if let Some(c) = &st.cancel {
            c.send_replace(true);
        }
        Ok(st.running)
    }

    pub fn suspend(&self, id: &str, on: bool) -> Result<(), Error> {
        if !self.0.state.lock().unwrap().contains_key(id) {
            return Err(Error::UnknownSource);
        }
        let key = format!("suspended:{id}");
        if on == !self.0.idx.meta(&key).is_empty() {
            return Ok(());
        }
        let since = if on {
            Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        } else {
            String::new()
        };
        Ok(self.0.idx.set_meta(&key, &since)?)
    }

    fn begin(&self, id: &str) -> Result<watch::Receiver<bool>, Error> {
        let mut state = self.0.state.lock().unwrap();
        let st = state.get_mut(id).ok_or(Error::UnknownSource)?;
        if st.running {
            return Err(Error::Busy);
        }
        let (tx, rx) = watch::channel(false);
        st.running = true;
        st.cancel = Some(tx);
        Ok(rx)
    }

    async fn run_started(
        &self,
        id: &str,
        full: bool,
        cancel: watch::Receiver<bool>,
    ) -> Result<Run, Error> {
        let src = self
            .0
            .sources
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .unwrap_or_default();
        let mut run = Run {
            full,
            started: now(),
            finished: now(),
            upserted: 0,
            deleted: 0,
            swept: 0,
            note: String::new(),
            error: String::new(),
        };
        log::info(
            "harvest started",
            &[("source", json!(id)), ("full", json!(full))],
        );
        let mut dropped = BTreeMap::new();
        let shutdown = self.0.shutdown.subscribe();
        let result = tokio::select! {
            r = self.harvest(&src, full, &mut run, &mut dropped) => r,
            _ = cancelled(cancel, shutdown) => Err(Error::Cancelled),
        };
        run.finished = now();
        if let Err(e) = &result {
            run.error = e.to_string();
        }
        {
            let mut state = self.0.state.lock().unwrap();
            let st = state.get_mut(id).unwrap();
            st.running = false;
            st.cancel = None;
            st.last = Some(run.clone());
            for (k, n) in dropped {
                *st.dropped.entry(k).or_default() += n;
            }
        }
        let mut attrs = vec![
            ("source", json!(id)),
            ("full", json!(full)),
            ("upserted", json!(run.upserted)),
            ("deleted", json!(run.deleted)),
            ("swept", json!(run.swept)),
            ("seconds", json!((run.finished - run.started).num_seconds())),
        ];
        match &result {
            Err(e) => {
                attrs.push(("err", json!(e.to_string())));
                log::error("harvest failed", &attrs);
            }
            Ok(()) => {
                attrs.push(("note", json!(run.note)));
                log::info("harvest finished", &attrs);
            }
        }
        result.map(|_| run)
    }

    async fn blocking<T: Send + 'static>(
        &self,
        f: impl FnOnce(&Index) -> crate::Result<T> + Send + 'static,
    ) -> Result<T, Error> {
        let idx = self.0.idx.clone();
        Ok(tokio::task::spawn_blocking(move || f(&idx)).await??)
    }

    async fn harvest(
        &self,
        src: &Source,
        full: bool,
        run: &mut Run,
        dropped: &mut BTreeMap<String, usize>,
    ) -> Result<(), Error> {
        let mut c = oai::Client::new(&src.oai_url, &self.0.agent);
        c.backoff = self.0.backoff;
        let sets = c.list_sets().await.map_err(|e| format!("ListSets: {e}"))?;
        let idx = &self.0.idx;
        let sid = src.id.clone();
        let before = self.blocking(move |x| x.count(&sid)).await?;
        let mut generation = idx.meta(&format!("gen:{}", src.id));
        if generation.is_empty() {
            generation = "0".into();
        }
        let mut from = String::new();
        if full {
            generation = (generation.parse::<u64>().unwrap_or(0) + 1).to_string();
            idx.set_meta(&format!("gen:{}", src.id), &generation)?;
        } else {
            let cur = idx.meta(&format!("cursor:{}", src.id));
            if !cur.is_empty() {
                let t = DateTime::parse_from_rfc3339(&cur)
                    .map_err(|e| format!("stored cursor {cur:?}: {e}"))?;
                from = (t.with_timezone(&Utc) - chrono::Duration::hours(24))
                    .format("%Y-%m-%d")
                    .to_string();
            }
        }

        let parser: oai::Parser = if src.metadata_prefix == "oai_dc" {
            record::from_oai_dc
        } else {
            record::from_dim
        };
        let mut pages = c.list_records(&src.metadata_prefix, &from, parser);
        while let Some(items) = pages.next().await? {
            let (mut ups, mut dels) = (Vec::new(), Vec::new());
            for item in items {
                let id = format!("{}:{}", src.id, local_id(&item.header.identifier));
                if item.header.deleted {
                    dels.push(id);
                    continue;
                }
                let Some((mut rec, drop)) = item.metadata else {
                    return Err(format!("{}: no metadata", item.header.identifier).into());
                };
                rec.id = id;
                rec.source = src.id.clone();
                rec.modified = item.header.datestamp;
                rec.is_part_of = set_names(&item.header.set_specs, &sets);
                for k in drop {
                    *dropped.entry(k).or_default() += 1;
                }
                ups.push(rec);
            }
            let (n_ups, n_dels, generation) = (ups.len(), dels.len(), generation.clone());
            self.blocking(move |x| x.apply(&ups, &dels, &generation))
                .await?;
            run.upserted += n_ups;
            run.deleted += n_dels;
        }
        idx.set_meta(&format!("cursor:{}", src.id), &pages.date)?;
        if !full {
            return Ok(());
        }
        let seen = run.upserted + run.deleted;
        if (seen as f64) < 0.9 * before as f64 {
            run.note = format!("sweep skipped: saw {seen} records but {before} were indexed");
            return Ok(());
        }
        let sid = src.id.clone();
        run.swept = self.blocking(move |x| x.sweep(&sid, &generation)).await?;
        Ok(())
    }

    pub fn status(&self) -> Result<Vec<SourceStatus>, Error> {
        let loc = *self.0.location.lock().unwrap();
        let mut out = Vec::new();
        for s in &self.0.sources {
            let idx = &self.0.idx;
            let docs = idx.count(&s.id)?;
            let next = loc.and_then(|loc| {
                let cron = schedule(&s.schedule).ok()?;
                cron.find_next_occurrence(&Utc::now().with_timezone(&loc), false)
                    .ok()
                    .map(|t| t.fixed_offset())
            });
            let state = self.0.state.lock().unwrap();
            let st = &state[&s.id];
            out.push(SourceStatus {
                id: s.id.clone(),
                name: s.name.clone(),
                docs,
                cursor: idx.meta(&format!("cursor:{}", s.id)),
                suspended: idx.meta(&format!("suspended:{}", s.id)),
                next,
                running: st.running,
                last: st.last.clone(),
                dropped: st.dropped.clone(),
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use super::*;
    use crate::index::tests::{Temp, temp_dir};
    use crate::oai::tests::serve;

    #[derive(Clone)]
    struct Rec(&'static str, &'static str, bool);

    #[derive(Default)]
    struct Fake {
        recs: Vec<Rec>,
        empty: bool,
        froms: Vec<String>,
        hold: bool,
    }

    const PAGE: usize = 2;

    fn respond(f: &StdMutex<Fake>, q: &str) -> String {
        let params: HashMap<String, String> = q
            .split('&')
            .filter_map(|p| p.split_once('='))
            .map(|(k, v)| (k.to_string(), v.replace("%3A", ":").replace("%2F", "/")))
            .collect();
        let token = params.get("resumptionToken").cloned().unwrap_or_default();
        while !token.is_empty() && f.lock().unwrap().hold {
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut f = f.lock().unwrap();
        let mut w = String::from(
            r#"<?xml version="1.0" encoding="UTF-8"?><OAI-PMH xmlns="http://www.openarchives.org/OAI/2.0/"><responseDate>2026-10-26T05:00:00Z</responseDate>"#,
        );
        if params.get("verb").map(String::as_str) == Some("ListSets") {
            w.push_str("<ListSets><set><setSpec>com_1_1</setSpec><setName>Fakultas Ekonomi</setName></set></ListSets></OAI-PMH>");
            return w;
        }
        if token.is_empty() {
            let from = params.get("from").cloned().unwrap_or_default();
            f.froms.push(from);
        }
        if f.empty || f.recs.is_empty() {
            w.push_str(r#"<error code="noRecordsMatch">no records</error></OAI-PMH>"#);
            return w;
        }
        let start: usize = token.parse().unwrap_or(0);
        let end = (start + PAGE).min(f.recs.len());
        w.push_str("<ListRecords>");
        for Rec(handle, title, deleted) in &f.recs[start..end] {
            if *deleted {
                w.push_str(&format!(r#"<record><header status="deleted"><identifier>oai:example:{handle}</identifier><datestamp>2026-10-20T00:00:00Z</datestamp></header></record>"#));
            } else {
                w.push_str(&format!(
                    r#"<record><header><identifier>oai:example:{handle}</identifier><datestamp>2026-10-20T00:00:00Z</datestamp><setSpec>com_1_1</setSpec><setSpec>col_unknown</setSpec></header><metadata><dim:dim xmlns:dim="http://www.dspace.org/xmlns/dspace/dim"><dim:field mdschema="dc" element="title">{title}</dim:field><dim:field mdschema="dc" element="date" qualifier="accessioned">2026-10-20</dim:field></dim:dim></metadata></record>"#
                ));
            }
        }
        let next = if end < f.recs.len() {
            end.to_string()
        } else {
            String::new()
        };
        w.push_str(&format!(
            "<resumptionToken>{next}</resumptionToken></ListRecords></OAI-PMH>"
        ));
        w
    }

    async fn setup(f: Arc<StdMutex<Fake>>) -> (Temp, Harvester, Arc<Index>) {
        let fake = f.clone();
        let url = serve(Arc::new(move |q: &str, _: &str| {
            (200, vec![], respond(&fake, q))
        }))
        .await;
        let (t, dir) = temp_dir();
        let idx = Arc::new(Index::open(&dir).unwrap());
        let src = Source {
            id: "repo".into(),
            name: "Repo".into(),
            oai_url: url,
            metadata_prefix: "dim".into(),
            schedule: "0 3 * * 1".into(),
        };
        (
            t,
            Harvester::with_backoff(
                idx.clone(),
                vec![src],
                "dssr/test",
                Duration::from_millis(1),
            ),
            idx,
        )
    }

    fn fake(recs: Vec<Rec>) -> Arc<StdMutex<Fake>> {
        Arc::new(StdMutex::new(Fake {
            recs,
            ..Default::default()
        }))
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn incremental_then_full() {
        let f = fake(vec![
            Rec("1/1", "Wisata ziarah", false),
            Rec("1/2", "Pakan jerami", false),
            Rec("1/3", "Withdrawn", true),
            Rec("1/4", "Strategi pemasaran", false),
        ]);
        let (_t, h, idx) = setup(f.clone()).await;

        let run = h.run("repo", false).await.unwrap();
        assert_eq!(
            (run.upserted, run.deleted, idx.count("repo").unwrap()),
            (3, 1, 3)
        );
        let r = idx.get("repo:1/1").unwrap().unwrap();
        assert_eq!(
            (r.title.as_str(), r.is_part_of.clone(), r.modified.as_str()),
            (
                "Wisata ziarah",
                vec!["Fakultas Ekonomi".to_string()],
                "2026-10-20T00:00:00Z"
            )
        );

        f.lock().unwrap().recs = vec![Rec("1/2", "Pakan jerami padi", false), Rec("1/1", "", true)];
        h.run("repo", false).await.unwrap();
        assert_eq!(f.lock().unwrap().froms[..2], ["", "2026-10-25"]);
        assert_eq!(
            idx.get("repo:1/2").unwrap().unwrap().title,
            "Pakan jerami padi"
        );
        assert_eq!(idx.count("repo").unwrap(), 2);

        f.lock().unwrap().recs = vec![
            Rec("1/2", "Pakan jerami padi", false),
            Rec("1/5", "Baru", false),
        ];
        let run = h.run("repo", true).await.unwrap();
        assert_eq!(
            (
                run.swept,
                idx.count("repo").unwrap(),
                f.lock().unwrap().froms[2].as_str()
            ),
            (1, 2, "")
        );

        let st = h.status().unwrap();
        assert_eq!(st[0].docs, 2);
        assert_eq!(st[0].cursor, "2026-10-26T05:00:00Z");
        assert_eq!(st[0].dropped["dc.date.accessioned"], 6);
        assert_eq!(st[0].last.as_ref().unwrap().error, "");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn full_sweep_guard() {
        let f = fake(vec![
            Rec("1/1", "a", false),
            Rec("1/2", "b", false),
            Rec("1/3", "c", false),
        ]);
        let (_t, h, idx) = setup(f.clone()).await;
        h.run("repo", true).await.unwrap();
        f.lock().unwrap().empty = true;
        let run = h.run("repo", true).await.unwrap();
        assert!(
            run.swept == 0 && !run.note.is_empty() && idx.count("repo").unwrap() == 3,
            "{run:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn busy_and_unknown() {
        let (_t, h, _) = setup(fake(vec![])).await;
        h.begin("repo").unwrap();
        assert_eq!(h.run("repo", false).await.err(), Some(Error::Busy));
        assert_eq!(h.start("nope", false).err(), Some(Error::UnknownSource));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancel_keeps_cursor() {
        let f = fake(vec![
            Rec("1/1", "a", false),
            Rec("1/2", "b", false),
            Rec("1/3", "c", false),
        ]);
        let (_t, h, idx) = setup(f.clone()).await;
        idx.set_meta("cursor:repo", "2026-01-01T00:00:00Z").unwrap();
        f.lock().unwrap().hold = true;
        h.start("repo", false).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(h.cancel("repo"), Ok(true));
        h.stop().await;
        f.lock().unwrap().hold = false;
        let st = h.status().unwrap();
        assert!(!st[0].running);
        assert_eq!(
            st[0].last.as_ref().unwrap().error,
            Error::Cancelled.to_string()
        );
        assert_eq!(st[0].cursor, "2026-01-01T00:00:00Z");
        assert_eq!(h.cancel("repo"), Ok(false));
        assert_eq!(h.cancel("nope"), Err(Error::UnknownSource));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn suspend_skips_schedule() {
        let (_t, h, idx) = setup(fake(vec![Rec("1/1", "a", false)])).await;
        h.suspend("repo", true).unwrap();
        assert_eq!(h.start_scheduled("repo").err(), Some(Error::Suspended));
        assert!(!h.status().unwrap()[0].suspended.is_empty());
        h.run("repo", false).await.unwrap();
        assert_eq!(idx.count("repo").unwrap(), 1);
        h.suspend("repo", false).unwrap();
        h.start_scheduled("repo").unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        h.stop().await;
        assert_eq!(h.suspend("nope", true).err(), Some(Error::UnknownSource));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn failed_run_keeps_cursor() {
        let f = fake(vec![Rec("1/1", "a", false)]);
        let (_t, h, idx) = setup(f.clone()).await;
        h.run("repo", false).await.unwrap();
        idx.set_meta("cursor:repo", "2026-01-01T00:00:00Z").unwrap();
        f.lock().unwrap().recs = vec![Rec("1/1", "<broken", false)];
        assert!(h.run("repo", false).await.is_err());
        assert_eq!(idx.meta("cursor:repo"), "2026-01-01T00:00:00Z");
    }

    #[test]
    fn helpers() {
        assert_eq!(
            local_id("oai:repository.example.ac.id:123456789/56"),
            "123456789/56"
        );
        let sets = HashMap::from([
            ("a".to_string(), "A".to_string()),
            ("b".to_string(), "A".to_string()),
            ("c".to_string(), String::new()),
        ]);
        assert_eq!(
            set_names(&["a".into(), "b".into(), "c".into(), "x".into()], &sets),
            ["A"]
        );
    }
}
