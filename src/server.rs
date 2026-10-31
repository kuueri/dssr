use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, RawQuery, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::config::{Config, decode_hex};
use crate::harvest::{self, Harvester, SourceStatus};
use crate::index::{Index, SearchQuery};
use crate::log;
use crate::searchlog::{self, SearchLog};

pub const OPENAPI: &str = include_str!("openapi.json");

pub struct Server {
    idx: Arc<Index>,
    h: Harvester,
    sources: Vec<String>,
    tokens: Vec<(String, Vec<u8>)>,
    version: String,
    started: DateTime<FixedOffset>,
    search_log: Option<Arc<SearchLog>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Status {
    pub version: String,
    pub started: DateTime<FixedOffset>,
    pub sources: Vec<SourceStatus>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub orphans: Vec<Orphan>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Orphan {
    pub id: String,
    pub docs: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Problem {
    pub r#type: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
}

type App = State<Arc<Server>>;

fn json_response(status: StatusCode, content_type: &'static str, body: impl Serialize) -> Response {
    let mut b = serde_json::to_vec(&body).unwrap_or_default();
    b.push(b'\n');
    (status, [(header::CONTENT_TYPE, content_type)], b).into_response()
}

fn ok(status: StatusCode, body: impl Serialize) -> Response {
    json_response(status, "application/json", body)
}

pub fn problem(status: StatusCode, detail: &str) -> Response {
    let p = Problem {
        r#type: "about:blank".into(),
        title: status.canonical_reason().unwrap_or("").into(),
        status: status.as_u16(),
        detail: detail.into(),
    };
    json_response(status, "application/problem+json", p)
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16)
                {
                    Ok(v) => {
                        out.push(v);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub struct Values(Vec<(String, String)>);

impl Values {
    pub fn parse(raw: &str) -> Values {
        Values(
            raw.split('&')
                .filter(|p| !p.is_empty())
                .map(|p| p.split_once('=').unwrap_or((p, "")))
                .map(|(k, v)| (decode(k), decode(v)))
                .collect(),
        )
    }

    fn get(&self, key: &str) -> &str {
        self.0
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    }

    fn all(&self, key: &str) -> Vec<String> {
        self.0
            .iter()
            .filter(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .collect()
    }
}

fn is_year(s: &str) -> bool {
    s.len() == 4 && s.bytes().all(|c| c.is_ascii_digit())
}

fn int_param(v: &Values, key: &str, default: i64) -> Option<i64> {
    match v.get(key) {
        "" => Some(default),
        s => s.parse().ok(),
    }
}

pub fn parse_query(v: &Values) -> Result<SearchQuery, &'static str> {
    let mut q = SearchQuery {
        q: v.get("q").trim().to_string(),
        types: v.all("type"),
        collections: v.all("collection"),
        languages: v.all("language"),
        ..Default::default()
    };
    if q.q.is_empty() {
        return Err("q is required");
    }
    if q.q.chars().count() > 200 {
        return Err("q is longer than 200 characters");
    }
    let y = v.get("year");
    if !y.is_empty() {
        let (from, to) = y.split_once('-').unwrap_or((y, y));
        if !is_year(from) || !is_year(to) || from > to {
            return Err("year must be YYYY or YYYY-YYYY");
        }
        (q.year_from, q.year_to) = (from.to_string(), to.to_string());
    }
    match v.get("sort") {
        "" | "relevance" => {}
        "newest" => q.newest = true,
        _ => return Err("sort must be relevance or newest"),
    }
    let page = int_param(v, "page", 1)
        .filter(|p| *p >= 1)
        .ok_or("page must be a positive integer")?;
    let limit = int_param(v, "limit", 10)
        .filter(|l| (1..=50).contains(l))
        .ok_or("limit must be between 1 and 50")?;
    if page > 1000 / limit {
        return Err("results beyond position 1000 are not served");
    }
    (q.page, q.limit) = (page as usize, limit as usize);
    Ok(q)
}

impl Server {
    pub fn new(
        idx: Arc<Index>,
        h: Harvester,
        cfg: &Config,
        version: &str,
        search_log: Option<Arc<SearchLog>>,
    ) -> Arc<Server> {
        Arc::new(Server {
            idx,
            h,
            sources: cfg.sources.iter().map(|s| s.id.clone()).collect(),
            tokens: cfg
                .clients
                .iter()
                .filter_map(|c| decode_hex(&c.token_sha256).map(|h| (c.name.clone(), h)))
                .collect(),
            version: version.to_string(),
            started: chrono::Local::now().fixed_offset(),
            search_log,
        })
    }

    fn client(&self, headers: &HeaderMap) -> Option<&str> {
        let value = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let tok = value.strip_prefix("Bearer ")?;
        if tok.is_empty() {
            return None;
        }
        let sum = Sha256::digest(tok.as_bytes());
        let mut matched = None;
        for (name, t) in &self.tokens {
            if sum.as_slice().ct_eq(t).unwrap_u8() == 1 {
                matched = Some(name.as_str());
            }
        }
        matched
    }

    fn targets(&self, v: &Values) -> Vec<String> {
        match v.get("source") {
            "" => self.sources.clone(),
            id => vec![id.to_string()],
        }
    }
}

pub fn api(s: Arc<Server>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok\n" }))
        .route(
            "/openapi.json",
            get(|| async { ([(header::CONTENT_TYPE, "application/json")], OPENAPI) }),
        )
        .route(
            "/v1/search",
            get(|st: App, h: HeaderMap, q: RawQuery| async move {
                let client = st.client(&h).map(str::to_string);
                if let Some(client) = client {
                    search(st, q, Some(client)).await
                } else {
                    unauthorized()
                }
            }),
        )
        .route(
            "/v1/records/{source}/{*handle}",
            get(
                |st: App, h: HeaderMap, p: Path<(String, String)>| async move {
                    if st.client(&h).is_some() {
                        record(st, p).await
                    } else {
                        unauthorized()
                    }
                },
            ),
        )
        .with_state(s)
}

pub fn admin(s: Arc<Server>) -> Router {
    Router::new()
        .route("/status", get(status))
        .route("/harvest", post(harvest_handler))
        .route("/cancel", post(cancel))
        .route(
            "/suspend",
            post(|st: App, q: RawQuery| suspend(st, q, true)),
        )
        .route(
            "/resume",
            post(|st: App, q: RawQuery| suspend(st, q, false)),
        )
        .route("/search", get(|st: App, q: RawQuery| search(st, q, None)))
        .route("/records/{source}/{*handle}", get(record))
        .with_state(s)
}

fn unauthorized() -> Response {
    let mut r = problem(StatusCode::UNAUTHORIZED, "missing or invalid bearer token");
    r.headers_mut()
        .insert(header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
    r
}

fn values(q: RawQuery) -> Values {
    Values::parse(q.0.as_deref().unwrap_or(""))
}

async fn search(State(s): App, q: RawQuery, client: Option<String>) -> Response {
    let q = match parse_query(&values(q)) {
        Ok(q) => q,
        Err(e) => return problem(StatusCode::BAD_REQUEST, e),
    };
    let idx = s.idx.clone();
    let query = q.clone();
    let text = q.q.clone();
    let started = std::time::Instant::now();
    match tokio::task::spawn_blocking(move || idx.search(&query).map_err(|e| e.to_string())).await {
        Ok(Ok(res)) => {
            if let (Some(log), Some(client)) = (s.search_log.as_ref(), client) {
                log.record(searchlog::event(
                    &q,
                    &res,
                    &client,
                    started.elapsed(),
                    log.hits,
                ));
            }
            ok(StatusCode::OK, res)
        }
        Ok(Err(e)) => {
            log::error("search failed", &[("q", json!(text)), ("err", json!(e))]);
            problem(StatusCode::INTERNAL_SERVER_ERROR, "search failed")
        }
        Err(e) => {
            log::error(
                "search failed",
                &[("q", json!(text)), ("err", json!(e.to_string()))],
            );
            problem(StatusCode::INTERNAL_SERVER_ERROR, "search failed")
        }
    }
}

async fn record(State(s): App, Path((source, handle)): Path<(String, String)>) -> Response {
    match s.idx.get(&format!("{source}:{handle}")) {
        Ok(Some(r)) => ok(StatusCode::OK, r),
        Ok(None) => problem(StatusCode::NOT_FOUND, "record not found"),
        Err(e) => {
            log::error("get failed", &[("err", json!(e.to_string()))]);
            problem(StatusCode::INTERNAL_SERVER_ERROR, "lookup failed")
        }
    }
}

async fn status(State(s): App) -> Response {
    let result: crate::Result<Status> = (|| {
        let sources = s.h.status().map_err(|e| e.to_string())?;
        let mut orphans = Vec::new();
        for id in s.idx.sources()? {
            if !s.sources.contains(&id) {
                orphans.push(Orphan {
                    docs: s.idx.count(&id)?,
                    id,
                });
            }
        }
        Ok(Status {
            version: s.version.clone(),
            started: s.started,
            sources,
            orphans,
        })
    })();
    match result {
        Ok(st) => ok(StatusCode::OK, st),
        Err(e) => problem(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

async fn cancel(State(s): App, q: RawQuery) -> Response {
    let mut cancelled = Vec::new();
    for id in s.targets(&values(q)) {
        match s.h.cancel(&id) {
            Err(_) => return problem(StatusCode::NOT_FOUND, &format!("unknown source {id}")),
            Ok(true) => cancelled.push(id),
            Ok(false) => {}
        }
    }
    ok(StatusCode::OK, json!({ "cancelled": cancelled }))
}

async fn suspend(State(s): App, q: RawQuery, on: bool) -> Response {
    let ids = s.targets(&values(q));
    for id in &ids {
        match s.h.suspend(id, on) {
            Err(harvest::Error::UnknownSource) => {
                return problem(StatusCode::NOT_FOUND, &format!("unknown source {id}"));
            }
            Err(e) => return problem(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
            Ok(()) => {}
        }
    }
    ok(StatusCode::OK, json!({ "sources": ids, "suspended": on }))
}

async fn harvest_handler(State(s): App, q: RawQuery) -> Response {
    let v = values(q);
    let ids = s.targets(&v);
    let full = v.get("full") == "true";
    for id in &ids {
        match s.h.start(id, full) {
            Err(harvest::Error::UnknownSource) => {
                return problem(StatusCode::NOT_FOUND, &format!("unknown source {id}"));
            }
            Err(harvest::Error::Busy) => {
                return problem(
                    StatusCode::CONFLICT,
                    &format!("a harvest is already running for {id}"),
                );
            }
            _ => {}
        }
    }
    ok(
        StatusCode::ACCEPTED,
        json!({ "started": ids, "full": full }),
    )
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;
    use crate::config::{Client, Source, encode_hex};
    use crate::index::SearchResult;
    use crate::index::tests::{Temp, temp_dir};
    use crate::record::Record;

    fn test_config() -> Config {
        let mut cfg: Config = serde_norway::from_str("listen: x").unwrap();
        cfg.clients = vec![Client {
            name: "test".into(),
            token_sha256: encode_hex(&Sha256::digest(b"secret")),
        }];
        cfg.sources = vec![Source {
            id: "repo".into(),
            schedule: "0 3 * * 1".into(),
            ..Default::default()
        }];
        cfg
    }

    fn setup() -> (Temp, std::path::PathBuf, Arc<Server>) {
        let (t, dir) = temp_dir();
        let idx = Arc::new(Index::open(&dir).unwrap());
        let r = Record {
            id: "repo:123456789/56".into(),
            source: "repo".into(),
            title: "Analisis potensi wisata ziarah".into(),
            issued: "2020".into(),
            r#type: vec!["Thesis".into()],
            ..Default::default()
        };
        idx.apply(&[r], &[], "1").unwrap();
        let cfg = test_config();
        let h = Harvester::new(idx.clone(), cfg.sources.clone(), "dssr/test");
        (t, dir, Server::new(idx, h, &cfg, "test", None))
    }

    async fn call(app: &Router, method: &str, target: &str, token: &str) -> (u16, String, String) {
        let mut req = Request::builder().method(method).uri(target);
        if !token.is_empty() {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let res = app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status().as_u16();
        let ct = res
            .headers()
            .get(header::CONTENT_TYPE)
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, ct, String::from_utf8(body.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn api_routes() {
        let (_t, _dir, s) = setup();
        let app = api(s);
        for (target, token, want) in [
            ("/healthz", "", 200),
            ("/openapi.json", "", 200),
            ("/v1/search?q=wisata", "", 401),
            ("/v1/search?q=wisata", "wrong", 401),
            ("/v1/search?q=wisata", "secret", 200),
            ("/v1/search", "secret", 400),
            ("/v1/search?q=wisata&limit=51", "secret", 400),
            ("/v1/search?q=wisata&page=0", "secret", 400),
            ("/v1/search?q=wisata&page=21&limit=50", "secret", 400),
            ("/v1/search?q=wisata&page=20&limit=50", "secret", 200),
            ("/v1/search?q=wisata&year=20-21", "secret", 400),
            ("/v1/search?q=wisata&year=2024-2020", "secret", 400),
            ("/v1/search?q=wisata&sort=oldest", "secret", 400),
            ("/v1/records/repo/123456789/56", "", 401),
            ("/v1/records/repo/123456789/56", "secret", 200),
            ("/v1/records/repo/123456789/99", "secret", 404),
        ] {
            let (status, ct, body) = call(&app, "GET", target, token).await;
            assert_eq!(status, want, "{target} with {token:?}: {body}");
            if status >= 400 {
                assert_eq!(ct, "application/problem+json", "{target}");
            }
        }
    }

    #[tokio::test]
    async fn api_search_is_logged() {
        let (_t, dir, s) = setup();
        let path = dir.join("search.log");
        let cfg = crate::config::SearchLog {
            output: path.to_str().unwrap().into(),
            hits: 1,
            max_size_mb: 64,
        };
        let (log, task) = crate::searchlog::start(&cfg).unwrap();
        let s = Server::new(
            s.idx.clone(),
            s.h.clone(),
            &test_config(),
            "test",
            Some(log),
        );
        call(&api(s.clone()), "GET", "/v1/search?q=wisata", "secret").await;
        call(&admin(s.clone()), "GET", "/search?q=wisata", "").await;
        call(&api(s.clone()), "GET", "/v1/search?q=wisata", "wrong").await;
        drop(s);
        task.await.unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            (lines[0]["client"].as_str(), lines[0]["q"].as_str()),
            (Some("test"), Some("wisata"))
        );
        assert_eq!(lines[0]["hits"][0]["id"], "repo:123456789/56");
    }

    #[tokio::test]
    async fn search_body() {
        let (_t, _dir, s) = setup();
        let target = "/v1/search?q=%22wisata+ziarah%22&type=Thesis&year=2020";
        let (_, _, body) = call(&api(s), "GET", target, "secret").await;
        let res: SearchResult = serde_json::from_str(&body).unwrap();
        assert_eq!(res.total, 1);
        assert_eq!(res.results[0].record.id, "repo:123456789/56");
        assert_eq!(res.results[0].record.issued, "2020");
        assert_eq!(res.facets["type"][0].value, "Thesis");
    }

    #[test]
    fn openapi_is_json() {
        let v: serde_json::Value = serde_json::from_str(OPENAPI).unwrap();
        assert_eq!(v["openapi"], "3.1.0");
    }

    #[tokio::test]
    async fn status_orphans() {
        let (_t, _dir, s) = setup();
        s.idx
            .apply(
                &[Record {
                    id: "old:1".into(),
                    source: "old".into(),
                    ..Default::default()
                }],
                &[],
                "1",
            )
            .unwrap();
        let (_, _, body) = call(&admin(s), "GET", "/status", "").await;
        let st: Status = serde_json::from_str(&body).unwrap();
        assert_eq!(
            st.orphans,
            [Orphan {
                id: "old".into(),
                docs: 1
            }]
        );
    }

    #[tokio::test]
    async fn admin_routes() {
        let (_t, _dir, s) = setup();
        let app = admin(s);
        let (_, _, body) = call(&app, "GET", "/status", "").await;
        assert!(!body.contains("orphans"));
        let st: Status = serde_json::from_str(&body).unwrap();
        assert_eq!((st.sources.len(), st.sources[0].docs), (1, 1));
        assert_eq!(call(&app, "POST", "/harvest?source=nope", "").await.0, 404);
        assert_eq!(
            call(&app, "GET", "/records/repo/123456789/56", "").await.0,
            200
        );
        for target in [
            "/cancel?source=nope",
            "/suspend?source=nope",
            "/resume?source=nope",
        ] {
            assert_eq!(call(&app, "POST", target, "").await.0, 404, "{target}");
        }
        assert_eq!(
            call(&app, "POST", "/cancel", "").await,
            (
                200,
                "application/json".into(),
                "{\"cancelled\":[]}\n".into()
            )
        );
        call(&app, "POST", "/suspend", "").await;
        let (_, _, body) = call(&app, "GET", "/status", "").await;
        let st: Status = serde_json::from_str(&body).unwrap();
        assert!(!st.sources[0].suspended.is_empty());
    }

    #[test]
    fn values_decode() {
        let v = Values::parse("q=%22a+b%22&type=X&type=Y%2FZ&bad=%zz");
        assert_eq!(v.get("q"), "\"a b\"");
        assert_eq!(v.all("type"), ["X", "Y/Z"]);
        assert_eq!(v.get("bad"), "%zz");
    }
}
