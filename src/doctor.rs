use std::ffi::CString;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::harvest;
use crate::oai;
use crate::server::Status;

#[derive(Debug, PartialEq)]
pub enum Level {
    Ok,
    Warn,
    Fail,
    Skip,
}

impl Level {
    fn label(&self) -> &'static str {
        match self {
            Level::Ok => "ok",
            Level::Warn => "warn",
            Level::Fail => "FAIL",
            Level::Skip => "skip",
        }
    }
}

#[derive(Debug)]
pub struct Check {
    pub level: Level,
    pub name: String,
    pub detail: String,
}

fn check(level: Level, name: impl Into<String>, detail: impl Into<String>) -> Check {
    Check {
        level,
        name: name.into(),
        detail: detail.into(),
    }
}

pub fn status_checks(st: &Status, binary: &str) -> Vec<Check> {
    let mut out = Vec::new();
    if st.version != binary {
        out.push(check(
            Level::Warn,
            "serve",
            format!("dssr {} runs; restart dssr to use {binary}", st.version),
        ));
    } else {
        out.push(check(Level::Ok, "serve", st.version.clone()));
    }
    let cancelled = harvest::Error::Cancelled.to_string();
    for s in &st.sources {
        let name = format!("source {}", s.id);
        if !s.suspended.is_empty() {
            out.push(check(
                Level::Warn,
                name,
                "suspended; run dssr resume".to_string(),
            ));
            continue;
        }
        if let Some(last) = &s.last
            && !last.error.is_empty()
        {
            if last.error == cancelled {
                out.push(check(
                    Level::Warn,
                    name,
                    "cancelled; run dssr harvest to start again".to_string(),
                ));
            } else {
                out.push(check(Level::Fail, name, last.error.clone()));
            }
            continue;
        }
        if s.docs == 0 && !s.running {
            out.push(check(
                Level::Warn,
                name,
                "no records; run dssr harvest".to_string(),
            ));
            continue;
        }
        let mut detail = format!("{} records", s.docs);
        if s.running {
            detail.push_str("; harvest running");
        }
        out.push(check(Level::Ok, name, detail));
    }
    for o in &st.orphans {
        out.push(check(
            Level::Warn,
            format!("orphan {}", o.id),
            format!("{} records; sudo dssr remove {}", o.docs, o.id),
        ));
    }
    out
}

pub fn probe_url(listen: &str) -> String {
    let (host, port) = listen.rsplit_once(':').unwrap_or((listen, "80"));
    let host = match host {
        "0.0.0.0" => "127.0.0.1",
        "[::]" => "[::1]",
        host => host,
    };
    format!("http://{host}:{port}/healthz")
}

pub fn identify(id: &str, url: &str, agent: &str, timeout: Duration) -> Check {
    let name = format!("oai {id}");
    let started = Instant::now();
    oai::install_tls();
    let client = match reqwest::blocking::Client::builder()
        .timeout(timeout)
        .user_agent(agent.to_string())
        .build()
    {
        Ok(c) => c,
        Err(e) => return check(Level::Fail, name, e.to_string()),
    };
    let response = match client.get(format!("{url}?verb=Identify")).send() {
        Ok(r) => r,
        Err(e) => return check(Level::Fail, name, e.to_string()),
    };
    let challenge = response
        .headers()
        .get("cf-mitigated")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("challenge"));
    if challenge {
        return check(
            Level::Fail,
            name,
            "Cloudflare challenge; allow this server in the firewall",
        );
    }
    if response.status() != reqwest::StatusCode::OK {
        return check(Level::Fail, name, response.status().to_string());
    }
    let body = match response.text() {
        Ok(b) => b,
        Err(e) => return check(Level::Fail, name, e.to_string()),
    };
    let doc = match roxmltree::Document::parse(&body) {
        Ok(d) => d,
        Err(e) => return check(Level::Fail, name, e.to_string()),
    };
    let root = doc.root_element();
    let is_identify = root.tag_name().name() == "OAI-PMH"
        && root
            .children()
            .any(|n| n.is_element() && n.tag_name().name() == "Identify");
    if !is_identify {
        return check(Level::Fail, name, "the response is not OAI-PMH Identify");
    }
    check(
        Level::Ok,
        name,
        format!("{:.1}s", started.elapsed().as_secs_f64()),
    )
}

pub fn index_check(index_path: &Path) -> Check {
    match index_path.try_exists() {
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            check(Level::Skip, "index", "sudo dssr doctor")
        }
        Err(e) => check(Level::Fail, "index", e.to_string()),
        Ok(false) => check(
            Level::Warn,
            "index",
            format!(
                "{} does not exist; dssr serve makes it",
                index_path.display()
            ),
        ),
        Ok(true) => {
            if index_path.join("index_meta.json").exists() {
                check(Level::Fail, "index", "run sudo dssr reindex")
            } else if index_path.join("tantivy/meta.json").exists() {
                check(Level::Ok, "index", index_path.display().to_string())
            } else {
                check(
                    Level::Warn,
                    "index",
                    format!(
                        "{} has no index yet; dssr serve makes it",
                        index_path.display()
                    ),
                )
            }
        }
    }
}

pub fn disk_check(index_path: &Path) -> Check {
    let mut target = index_path;
    loop {
        let path = match CString::new(target.to_string_lossy().as_bytes()) {
            Ok(p) => p,
            Err(e) => return check(Level::Fail, "disk", e.to_string()),
        };
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(path.as_ptr(), &mut stat) } == 0 {
            let free = stat.f_bavail * stat.f_frsize;
            let detail = format!("{:.1} GiB free", free as f64 / 1024.0 / 1024.0 / 1024.0);
            return if free < 1024 * 1024 * 1024 {
                check(Level::Warn, "disk", detail)
            } else {
                check(Level::Ok, "disk", detail)
            };
        }
        let e = std::io::Error::last_os_error();
        if e.raw_os_error() == Some(libc::EACCES) {
            return check(Level::Skip, "disk", "sudo dssr doctor");
        }
        if e.raw_os_error() == Some(libc::ENOENT)
            && let Some(parent) = target.parent()
        {
            target = parent;
            continue;
        }
        return check(Level::Fail, "disk", e.to_string());
    }
}

pub fn print(checks: &[Check]) -> usize {
    for c in checks {
        println!("{:<5} {:<16} {}", c.level.label(), c.name, c.detail);
    }
    checks.iter().filter(|c| c.level == Level::Fail).count()
}

fn finish(checks: &[Check]) -> crate::Result<()> {
    let n = print(checks);
    if n > 0 {
        Err(format!("doctor found {n} problems").into())
    } else {
        Ok(())
    }
}

pub fn doctor(args: &[String]) -> crate::Result<()> {
    let mut fs = crate::new_flags("doctor");
    if !crate::parse(&mut fs, args).is_empty() {
        return Err("doctor takes no arguments".into());
    }
    let cfg_path = fs.get("config").to_string();
    let mut checks = Vec::new();

    match crate::latest_release(crate::LATEST_RELEASE_URL) {
        Ok(tag) if crate::newer(&tag, crate::build_version()) => checks.push(check(
            Level::Warn,
            "version",
            format!("dssr {tag} is available; run sudo dssr update"),
        )),
        Ok(_) => checks.push(check(Level::Ok, "version", crate::version())),
        Err(e) => checks.push(check(Level::Skip, "version", e.to_string())),
    }

    let cfg = match Config::load(&cfg_path) {
        Ok(cfg) => {
            checks.push(check(Level::Ok, "config", cfg_path.clone()));
            cfg
        }
        Err(e) => {
            checks.push(check(Level::Fail, "config", e.to_string()));
            return finish(&checks);
        }
    };

    match crate::systemctl() {
        None => checks.push(check(Level::Skip, "service", "systemctl not found")),
        Some(sc) => {
            if !crate::is_active(&sc) {
                checks.push(check(
                    Level::Fail,
                    "service",
                    "dssr.service is not active; run sudo journalctl -u dssr -n 20 --no-pager",
                ));
            } else if !Command::new(&sc)
                .args(["is-enabled", "--quiet", "dssr.service"])
                .status()
                .is_ok_and(|s| s.success())
            {
                checks.push(check(Level::Warn, "service", "dssr.service is not enabled"));
            } else {
                checks.push(check(Level::Ok, "service", "dssr.service is active"));
            }
        }
    }

    let mut status = None;
    match crate::call(&cfg_path, "GET", "/status") {
        Ok(body) => match serde_json::from_slice::<Status>(&body) {
            Ok(st) => {
                checks.push(check(Level::Ok, "socket", cfg.admin_socket.clone()));
                status = Some(st);
            }
            Err(e) => checks.push(check(Level::Fail, "socket", e.to_string())),
        },
        Err(e) => checks.push(check(Level::Fail, "socket", e.to_string())),
    }
    if let Some(st) = &status {
        checks.extend(status_checks(st, crate::version()));
    } else {
        checks.push(check(Level::Skip, "serve", "dssr serve does not answer"));
    }

    oai::install_tls();
    let url = probe_url(&cfg.listen);
    match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(client) => match client.get(&url).send() {
            Ok(res) if res.status() != reqwest::StatusCode::OK => {
                checks.push(check(Level::Fail, "api", res.status().to_string()));
            }
            Ok(res) => match res.text() {
                Ok(body) if body == "ok\n" => {
                    checks.push(check(Level::Ok, "api", url.clone()));
                }
                Ok(body) => checks.push(check(
                    Level::Fail,
                    "api",
                    format!("unexpected body {body:?}"),
                )),
                Err(e) => checks.push(check(Level::Fail, "api", e.to_string())),
            },
            Err(e) => checks.push(check(Level::Fail, "api", e.to_string())),
        },
        Err(e) => checks.push(check(Level::Fail, "api", e.to_string())),
    }

    checks.push(index_check(Path::new(&cfg.index_path)));
    checks.push(disk_check(Path::new(&cfg.index_path)));

    let agent = format!("dssr/{}", crate::version());
    for source in &cfg.sources {
        checks.push(identify(
            &source.id,
            &source.oai_url,
            &agent,
            Duration::from_secs(30),
        ));
    }

    finish(&checks)
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Arc;

    use super::*;

    #[test]
    fn status_checks_levels() {
        let st: Status = serde_json::from_value(serde_json::json!({
            "version": "0.0.10", "started": "2026-10-31T00:00:00+07:00",
            "sources": [
                {"id": "a", "name": "A", "docs": 5, "running": false},
                {"id": "b", "name": "B", "docs": 0, "running": false, "suspended": "2026-10-31T00:00:00Z"},
                {"id": "c", "name": "C", "docs": 3, "running": false, "last": {"full": false,
                  "started": "2026-10-31T00:00:00+07:00", "finished": "2026-10-31T00:00:00+07:00",
                  "upserted": 0, "deleted": 0, "swept": 0, "error": "GET x: 403 Forbidden"}},
                {"id": "d", "name": "D", "docs": 0, "running": true}
            ],
            "orphans": [{"id": "old", "docs": 2}]
        })).unwrap();
        let c = status_checks(&st, "0.0.11");
        let levels: Vec<(&str, &Level)> = c.iter().map(|c| (c.name.as_str(), &c.level)).collect();
        assert_eq!(
            levels,
            [
                ("serve", &Level::Warn),
                ("source a", &Level::Ok),
                ("source b", &Level::Warn),
                ("source c", &Level::Fail),
                ("source d", &Level::Ok),
                ("orphan old", &Level::Warn),
            ]
        );
        assert!(c[0].detail.contains("restart dssr"));
        assert!(c[5].detail.contains("sudo dssr remove old"));
        assert_eq!(status_checks(&st, "0.0.10")[0].level, Level::Ok);
    }

    #[test]
    fn probe_url_loopback() {
        assert_eq!(probe_url("127.0.0.1:8080"), "http://127.0.0.1:8080/healthz");
        assert_eq!(probe_url("0.0.0.0:8080"), "http://127.0.0.1:8080/healthz");
        assert_eq!(probe_url("[::]:9000"), "http://[::1]:9000/healthz");
        assert_eq!(probe_url("10.0.0.5:8080"), "http://10.0.0.5:8080/healthz");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn identify_checks() {
        let ok = crate::oai::tests::serve(Arc::new(|q: &str, _: &str| {
            assert_eq!(q, "verb=Identify");
            (
                200,
                vec![],
                r#"<OAI-PMH xmlns="http://www.openarchives.org/OAI/2.0/"><Identify/></OAI-PMH>"#
                    .into(),
            )
        }))
        .await;
        let cf = crate::oai::tests::serve(Arc::new(|_: &str, _: &str| {
            (
                403,
                vec![("cf-mitigated".into(), "challenge".into())],
                String::new(),
            )
        }))
        .await;
        let html =
            crate::oai::tests::serve(Arc::new(|_: &str, _: &str| (200, vec![], "<html/>".into())))
                .await;
        let r = tokio::task::spawn_blocking(move || {
            let t = Duration::from_secs(5);
            [
                identify("a", &ok, "dssr/test", t),
                identify("b", &cf, "dssr/test", t),
                identify("c", &html, "dssr/test", t),
            ]
        })
        .await
        .unwrap();
        assert_eq!(
            r.iter().map(|c| &c.level).collect::<Vec<_>>(),
            [&Level::Ok, &Level::Fail, &Level::Fail]
        );
        assert!(r[1].detail.contains("Cloudflare challenge"));
    }

    #[test]
    fn index_check_states() {
        let (_t, dir) = crate::index::tests::temp_dir();
        assert_eq!(index_check(&dir).level, Level::Warn);
        std::fs::create_dir_all(dir.join("tantivy")).unwrap();
        std::fs::write(dir.join("tantivy/meta.json"), "{}").unwrap();
        assert_eq!(index_check(&dir).level, Level::Ok);
        std::fs::write(dir.join("index_meta.json"), "{}").unwrap();
        assert_eq!(index_check(&dir).level, Level::Fail);
    }

    #[test]
    fn index_check_denied() {
        let (_t, dir) = crate::index::tests::temp_dir();
        std::fs::create_dir_all(dir.join("locked/index")).unwrap();
        std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        let c = index_check(&dir.join("locked/index"));
        std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        if !crate::is_root() {
            assert_eq!(c.level, Level::Skip);
        }
    }
}
