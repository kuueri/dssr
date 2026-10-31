use std::str::FromStr;

use serde::{Deserialize, Deserializer};

use crate::Result;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub listen: String,
    #[serde(default)]
    pub index_path: String,
    #[serde(default)]
    pub admin_socket: String,
    #[serde(default)]
    pub timezone: String,
    #[serde(default, deserialize_with = "list")]
    pub clients: Vec<Client>,
    #[serde(default, deserialize_with = "list")]
    pub sources: Vec<Source>,
    #[serde(default)]
    pub search_log: Option<SearchLog>,
    #[serde(skip, default = "utc")]
    pub location: chrono_tz::Tz,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Client {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub token_sha256: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub oai_url: String,
    #[serde(default)]
    pub metadata_prefix: String,
    #[serde(default)]
    pub schedule: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchLog {
    #[serde(default)]
    pub output: String,
    #[serde(default = "default_hits")]
    pub hits: usize,
    #[serde(default = "default_max_size_mb")]
    pub max_size_mb: u64,
}

fn default_hits() -> usize {
    10
}

fn default_max_size_mb() -> u64 {
    64
}

fn utc() -> chrono_tz::Tz {
    chrono_tz::UTC
}

fn list<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> std::result::Result<Vec<T>, D::Error> {
    Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
}

pub fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

pub fn encode_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn valid_source_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

pub fn schedule(s: &str) -> std::result::Result<croner::Cron, String> {
    croner::Cron::from_str(s).map_err(|e| e.to_string())
}

impl Config {
    pub fn load(path: &str) -> Result<Config> {
        let b = std::fs::read_to_string(path)?;
        let mut c: Config = serde_norway::from_str(&b).map_err(|e| format!("{path}: {e}"))?;
        c.validate().map_err(|e| format!("{path}: {e}"))?;
        Ok(c)
    }

    fn validate(&mut self) -> std::result::Result<(), String> {
        if self.listen.is_empty() || self.index_path.is_empty() || self.admin_socket.is_empty() {
            return Err("listen, index_path, and admin_socket are required".into());
        }
        if self.admin_socket.len() > 107 {
            return Err(
                "admin_socket path must be at most 107 bytes, the unix socket limit".into(),
            );
        }
        self.location = match self.timezone.as_str() {
            "" | "UTC" => chrono_tz::UTC,
            tz => chrono_tz::Tz::from_str(tz)
                .map_err(|_| format!("timezone: unknown time zone {tz}"))?,
        };
        for cl in &self.clients {
            if decode_hex(&cl.token_sha256).is_none_or(|h| h.len() != 32) {
                return Err(format!(
                    "client {:?}: token_sha256 must be 64 hex characters",
                    cl.name
                ));
            }
        }
        let mut seen = std::collections::HashSet::new();
        for s in &mut self.sources {
            if !valid_source_id(&s.id) {
                return Err(format!(
                    "source id {:?} must match ^[a-z0-9][a-z0-9-]*$",
                    s.id
                ));
            }
            if !seen.insert(s.id.clone()) {
                return Err(format!("source id {:?} is duplicated", s.id));
            }
            let url_ok = reqwest::Url::parse(&s.oai_url).is_ok_and(|u| {
                matches!(u.scheme(), "http" | "https")
                    && u.host_str().is_some_and(|h| !h.is_empty())
            });
            if !url_ok {
                return Err(format!(
                    "source {:?}: oai_url must be an http or https URL",
                    s.id
                ));
            }
            if s.metadata_prefix.is_empty() {
                s.metadata_prefix = "oai_dc".into();
            }
            if s.metadata_prefix != "dim" && s.metadata_prefix != "oai_dc" {
                return Err(format!(
                    "source {:?}: metadata_prefix must be dim or oai_dc, the mapped formats",
                    s.id
                ));
            }
            if s.schedule.split_whitespace().count() != 5 && !s.schedule.starts_with('@') {
                return Err(format!(
                    "source {:?}: schedule: expected exactly 5 fields, found {}: {}",
                    s.id,
                    s.schedule.split_whitespace().count(),
                    s.schedule
                ));
            }
            schedule(&s.schedule).map_err(|e| format!("source {:?}: schedule: {e}", s.id))?;
        }
        if let Some(log) = &self.search_log {
            if log.output != "stdout" && !std::path::Path::new(&log.output).is_absolute() {
                return Err("search_log: output must be stdout or an absolute file path".into());
            }
            if !(1..=50).contains(&log.hits) {
                return Err("search_log: hits must be between 1 and 50".into());
            }
            if log.max_size_mb < 1 {
                return Err("search_log: max_size_mb must be at least 1".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const VALID: &str = "listen: 127.0.0.1:8080
index_path: /var/lib/dssr/index
admin_socket: /run/dssr/admin.sock
timezone: Asia/Jakarta
clients:
  - name: library-chatbot
    token_sha256: 9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08
sources:
  - id: repo
    name: Example Repository
    oai_url: https://repository.example.org/oai/request
    metadata_prefix: dim
    schedule: \"0 3 * * 1\"
";

    fn load(s: &str) -> Result<Config> {
        let (_t, dir) = crate::index::tests::temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("dssr.yaml");
        std::fs::write(&p, s).unwrap();
        Config::load(p.to_str().unwrap())
    }

    #[test]
    fn valid() {
        let c = load(VALID).unwrap();
        assert_eq!(c.location, chrono_tz::Asia::Jakarta);
        assert_eq!(c.sources[0].id, "repo");
        assert_eq!(c.clients.len(), 1);
    }

    #[test]
    fn default_prefix() {
        let c = load(&VALID.replacen("    metadata_prefix: dim\n", "", 1)).unwrap();
        assert_eq!(c.sources[0].metadata_prefix, "oai_dc");
    }

    #[test]
    fn no_sources() {
        let head = VALID.split("sources:").next().unwrap();
        assert!(
            load(&format!("{head}# sources:\n#   - id: repo\n"))
                .unwrap()
                .sources
                .is_empty()
        );
        assert!(
            load(&format!("{head}sources:\n"))
                .unwrap()
                .sources
                .is_empty()
        );
    }

    #[test]
    fn search_log_config() {
        let base = "listen: x\nindex_path: /i\nadmin_socket: /s\n";
        let load = |extra: &str| {
            let mut c: Config = serde_norway::from_str(&format!("{base}{extra}")).unwrap();
            c.validate().map(|_| c.search_log)
        };
        assert!(load("").unwrap().is_none());
        let s = load("search_log:\n  output: stdout\n").unwrap().unwrap();
        assert_eq!(
            (s.output.as_str(), s.hits, s.max_size_mb),
            ("stdout", 10, 64)
        );
        assert!(load("search_log:\n  output: /var/log/dssr/search.log\n  hits: 50\n").is_ok());
        for bad in [
            "search_log:\n  output: search.log\n",
            "search_log:\n  output: \"\"\n",
            "search_log:\n  output: stdout\n  hits: 0\n",
            "search_log:\n  output: stdout\n  hits: 51\n",
            "search_log:\n  output: stdout\n  max_size_mb: 0\n",
        ] {
            assert!(load(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn rejects() {
        let long = format!("/run/{}/", "d".repeat(100));
        for (old, new, want) in [
            ("timezone:", "timezon:", "unknown field"),
            ("9f86d081", "zz86d081", "token_sha256"),
            ("\"0 3 * * 1\"", "\"every monday\"", "schedule"),
            ("\"0 3 * * 1\"", "\"0 0 3 * * 1\"", "schedule"),
            (
                "metadata_prefix: dim",
                "metadata_prefix: marcxml",
                "metadata_prefix",
            ),
            ("id: repo", "id: Repo:1", "source id"),
            ("https://repository", "ftp://repository", "oai_url"),
            ("/run/dssr/", long.as_str(), "admin_socket"),
        ] {
            let err = load(&VALID.replacen(old, new, 1))
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default();
            assert!(
                err.contains(want),
                "{old} -> {new}: want {want:?}, got {err:?}"
            );
        }
    }
}
