use std::collections::HashMap;
use std::time::Duration;

use crate::Result;
use crate::record::Record;

pub type Parser = fn(roxmltree::Node) -> (Record, Vec<String>);

pub struct Client {
    base_url: String,
    http: reqwest::Client,
    pub retries: u32,
    pub backoff: Duration,
}

pub struct Header {
    pub identifier: String,
    pub datestamp: String,
    pub set_specs: Vec<String>,
    pub deleted: bool,
}

pub struct Item {
    pub header: Header,
    pub metadata: Option<(Record, Vec<String>)>,
}

#[derive(Debug)]
pub struct OaiError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for OaiError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "oai-pmh {}: {}", self.code, self.message.trim())
    }
}

impl std::error::Error for OaiError {}

pub fn query_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn encode_query(pairs: &[(&str, &str)]) -> String {
    let mut pairs = pairs.to_vec();
    pairs.sort_by_key(|p| p.0);
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", query_escape(k), query_escape(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn child<'a, 'i>(n: roxmltree::Node<'a, 'i>, name: &str) -> Option<roxmltree::Node<'a, 'i>> {
    n.children()
        .find(|c| c.is_element() && c.tag_name().name() == name)
}

fn children<'a, 'i>(
    n: roxmltree::Node<'a, 'i>,
    name: &'static str,
) -> impl Iterator<Item = roxmltree::Node<'a, 'i>> {
    n.children()
        .filter(move |c| c.is_element() && c.tag_name().name() == name)
}

fn text(n: Option<roxmltree::Node>) -> String {
    n.map(|n| {
        n.children()
            .filter(|c| c.is_text())
            .filter_map(|c| c.text())
            .collect()
    })
    .unwrap_or_default()
}

fn first_error(root: roxmltree::Node) -> Option<OaiError> {
    child(root, "error").map(|e| OaiError {
        code: e.attribute("code").unwrap_or("").to_string(),
        message: text(Some(e)),
    })
}

enum Failure {
    Retry(String, Duration),
    Fatal(String),
}

fn retry_after(v: &str) -> Duration {
    let secs = match v.trim().parse::<i64>() {
        Ok(s) if s > 0 => s,
        _ => chrono::DateTime::parse_from_rfc2822(v)
            .map(|t| (t.timestamp() - chrono::Utc::now().timestamp()).max(0))
            .unwrap_or(0),
    };
    Duration::from_secs(secs.clamp(0, 600) as u64)
}

pub fn install_tls() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

impl Client {
    pub fn new(base_url: &str, user_agent: &str) -> Client {
        install_tls();
        let http = reqwest::Client::builder()
            .user_agent(user_agent)
            .timeout(Duration::from_secs(60))
            .build()
            .expect("http client");
        Client {
            base_url: base_url.to_string(),
            http,
            retries: 3,
            backoff: Duration::from_secs(2),
        }
    }

    async fn try_get(&self, url: &str) -> std::result::Result<String, Failure> {
        let res = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| Failure::Retry(e.to_string(), Duration::ZERO))?;
        let status = res.status();
        if status.is_server_error() {
            let after = res
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .map(retry_after)
                .unwrap_or_default();
            return Err(Failure::Retry(format!("GET {url}: {status}"), after));
        }
        if status != reqwest::StatusCode::OK {
            return Err(Failure::Fatal(format!("GET {url}: {status}")));
        }
        let body = res
            .text()
            .await
            .map_err(|e| Failure::Retry(format!("GET {url}: {e}"), Duration::ZERO))?;
        if let Err(e) = parse(&body) {
            return Err(Failure::Retry(format!("GET {url}: {e}"), Duration::ZERO));
        }
        Ok(body)
    }

    async fn get(&self, query: &str) -> Result<String> {
        let url = format!("{}?{query}", self.base_url);
        let mut attempt = 0;
        loop {
            match self.try_get(&url).await {
                Ok(body) => return Ok(body),
                Err(Failure::Fatal(e)) => return Err(e.into()),
                Err(Failure::Retry(e, _)) if attempt == self.retries => return Err(e.into()),
                Err(Failure::Retry(_, after)) => {
                    let wait = if after.is_zero() {
                        self.backoff * (1 << attempt)
                    } else {
                        after
                    };
                    tokio::time::sleep(wait).await;
                }
            }
            attempt += 1;
        }
    }

    pub fn list_records(&self, prefix: &str, from: &str, parser: Parser) -> Records<'_> {
        let mut q = vec![("verb", "ListRecords"), ("metadataPrefix", prefix)];
        if !from.is_empty() {
            q.push(("from", from));
        }
        Records {
            client: self,
            query: Some(encode_query(&q)),
            parser,
            date: String::new(),
        }
    }

    pub async fn list_sets(&self) -> Result<HashMap<String, String>> {
        let mut sets = HashMap::new();
        let mut query = encode_query(&[("verb", "ListSets")]);
        loop {
            let body = self.get(&query).await?;
            let doc = parse(&body)?;
            let root = doc.root_element();
            if let Some(e) = first_error(root) {
                if e.code == "noSetHierarchy" {
                    return Ok(sets);
                }
                return Err(e.into());
            }
            let list = child(root, "ListSets");
            for s in list.into_iter().flat_map(|l| children(l, "set")) {
                sets.insert(
                    text(child(s, "setSpec")),
                    text(child(s, "setName")).trim().to_string(),
                );
            }
            let tok = text(list.and_then(|l| child(l, "resumptionToken")));
            if tok.trim().is_empty() {
                return Ok(sets);
            }
            query = encode_query(&[("verb", "ListSets"), ("resumptionToken", tok.trim())]);
        }
    }
}

fn parse(body: &str) -> std::result::Result<roxmltree::Document<'_>, roxmltree::Error> {
    roxmltree::Document::parse_with_options(
        body,
        roxmltree::ParsingOptions {
            allow_dtd: true,
            ..Default::default()
        },
    )
}

pub struct Records<'c> {
    client: &'c Client,
    query: Option<String>,
    parser: Parser,
    pub date: String,
}

impl Records<'_> {
    pub async fn next(&mut self) -> Result<Option<Vec<Item>>> {
        let Some(query) = self.query.take() else {
            return Ok(None);
        };
        let body = self.client.get(&query).await?;
        let doc = parse(&body)?;
        let root = doc.root_element();
        if self.date.is_empty() {
            self.date = text(child(root, "responseDate")).trim().to_string();
        }
        if let Some(e) = first_error(root) {
            if e.code == "noRecordsMatch" {
                return Ok(None);
            }
            return Err(e.into());
        }
        let list = child(root, "ListRecords");
        let mut items = Vec::new();
        for r in list.into_iter().flat_map(|l| children(l, "record")) {
            let h = child(r, "header");
            let header = Header {
                identifier: text(h.and_then(|h| child(h, "identifier"))),
                datestamp: text(h.and_then(|h| child(h, "datestamp"))),
                set_specs: h
                    .into_iter()
                    .flat_map(|h| children(h, "setSpec"))
                    .map(|s| text(Some(s)))
                    .collect(),
                deleted: h.and_then(|h| h.attribute("status")) == Some("deleted"),
            };
            let metadata = child(r, "metadata")
                .and_then(|m| m.children().find(|c| c.is_element()))
                .map(self.parser);
            items.push(Item { header, metadata });
        }
        let tok = text(list.and_then(|l| child(l, "resumptionToken")));
        if !tok.trim().is_empty() {
            self.query = Some(encode_query(&[
                ("verb", "ListRecords"),
                ("resumptionToken", tok.trim()),
            ]));
        }
        Ok(Some(items))
    }
}

#[cfg(test)]
pub mod tests {
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::record::from_dim;

    pub type Handler =
        Arc<dyn Fn(&str, &str) -> (u16, Vec<(String, String)>, String) + Send + Sync>;

    pub async fn serve(handler: Handler) -> String {
        let ln = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = ln.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut conn, _)) = ln.accept().await else {
                    return;
                };
                let handler = handler.clone();
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match conn.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let head = String::from_utf8_lossy(&buf).to_string();
                    let target = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                    let agent = head
                        .lines()
                        .find_map(|l| {
                            l.strip_prefix("user-agent: ")
                                .or_else(|| l.strip_prefix("User-Agent: "))
                        })
                        .unwrap_or("")
                        .to_string();
                    let query = target
                        .split_once('?')
                        .map(|p| p.1.to_string())
                        .unwrap_or_default();
                    let (status, headers, body) =
                        tokio::task::spawn_blocking(move || handler(&query, &agent))
                            .await
                            .unwrap();
                    let mut out = format!(
                        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
                        body.len()
                    );
                    for (k, v) in headers {
                        out.push_str(&format!("{k}: {v}\r\n"));
                    }
                    out.push_str("\r\n");
                    out.push_str(&body);
                    let _ = conn.write_all(out.as_bytes()).await;
                });
            }
        });
        format!("http://{addr}")
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("testdata/oai/{name}")).unwrap()
    }

    fn client(url: &str) -> Client {
        let mut c = Client::new(url, "dssr/test");
        c.backoff = Duration::from_millis(1);
        c
    }

    #[tokio::test]
    async fn list_records_follows_resumption_token() {
        let queries = Arc::new(Mutex::new(Vec::new()));
        let seen = queries.clone();
        let url = serve(Arc::new(move |q: &str, agent: &str| {
            assert_eq!(agent, "dssr/test");
            seen.lock().unwrap().push(q.to_string());
            let name = if q.contains("resumptionToken=dim%2F%2F%2F%2F100") {
                "listrecords-2.xml"
            } else {
                "listrecords-1.xml"
            };
            (200, vec![], fixture(name))
        }))
        .await;
        let c = client(&url);
        let mut pages = c.list_records("dim", "2026-10-25T05:00:00Z", from_dim);
        let mut items = Vec::new();
        while let Some(p) = pages.next().await.unwrap() {
            items.extend(p);
        }
        assert_eq!(pages.date, "2026-10-26T05:00:00Z");
        assert_eq!(items.len(), 3);
        assert!(!items[0].header.deleted && items[1].header.deleted);
        assert_eq!(
            items[0].header.set_specs,
            ["com_123456789_1", "col_123456789_2"]
        );
        assert_eq!(
            items[0].metadata.as_ref().unwrap().0.title,
            "Analisis potensi wisata ziarah Malang Raya"
        );
        assert_eq!(
            queries.lock().unwrap()[0],
            "from=2026-10-25T05%3A00%3A00Z&metadataPrefix=dim&verb=ListRecords"
        );
    }

    #[tokio::test]
    async fn no_records_match() {
        let url = serve(Arc::new(|_: &str, _: &str| {
            (200, vec![], fixture("norecords.xml"))
        }))
        .await;
        let c = client(&url);
        let mut pages = c.list_records("dim", "", from_dim);
        assert!(pages.next().await.unwrap().is_none());
        assert_eq!(pages.date, "2026-10-26T06:00:00Z");
    }

    #[tokio::test]
    async fn retry_after_503() {
        let calls = Arc::new(Mutex::new(0));
        let n = calls.clone();
        let url = serve(Arc::new(move |_: &str, _: &str| {
            let mut n = n.lock().unwrap();
            *n += 1;
            if *n == 1 {
                (503, vec![("Retry-After".into(), "1".into())], String::new())
            } else {
                (200, vec![], fixture("norecords.xml"))
            }
        }))
        .await;
        let start = std::time::Instant::now();
        client(&url)
            .list_records("dim", "", from_dim)
            .next()
            .await
            .unwrap();
        assert_eq!(*calls.lock().unwrap(), 2);
        assert!(start.elapsed() >= Duration::from_secs(1));
    }

    #[tokio::test]
    async fn gives_up_after_retries() {
        let calls = Arc::new(Mutex::new(0));
        let n = calls.clone();
        let url = serve(Arc::new(move |_: &str, _: &str| {
            *n.lock().unwrap() += 1;
            (502, vec![], String::new())
        }))
        .await;
        assert!(
            client(&url)
                .list_records("dim", "", from_dim)
                .next()
                .await
                .is_err()
        );
        assert_eq!(*calls.lock().unwrap(), 4);
    }

    #[tokio::test]
    async fn no_retry_on_404() {
        let calls = Arc::new(Mutex::new(0));
        let n = calls.clone();
        let url = serve(Arc::new(move |_: &str, _: &str| {
            *n.lock().unwrap() += 1;
            (404, vec![], String::new())
        }))
        .await;
        assert!(client(&url).list_sets().await.is_err());
        assert_eq!(*calls.lock().unwrap(), 1);
    }

    #[tokio::test]
    async fn list_sets() {
        let url = serve(Arc::new(|_: &str, _: &str| {
            (200, vec![], fixture("listsets.xml"))
        }))
        .await;
        let sets = client(&url).list_sets().await.unwrap();
        assert_eq!(sets.len(), 2);
        assert_eq!(sets["com_123456789_1"], "Fakultas Ekonomi");
    }
}
