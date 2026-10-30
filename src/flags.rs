use std::collections::BTreeMap;

enum Kind {
    Text(String),
    Bool(bool),
    Multi(Vec<String>),
}

struct Flag {
    kind: Kind,
    default: String,
    usage: String,
}

pub struct FlagSet {
    help: String,
    flags: BTreeMap<String, Flag>,
}

pub enum Parsed {
    Ok(Vec<String>),
    Help,
    Error(String),
}

impl FlagSet {
    pub fn new(help: &str) -> FlagSet {
        FlagSet {
            help: help.to_string(),
            flags: BTreeMap::new(),
        }
    }

    pub fn text(&mut self, name: &str, default: &str, usage: &str) {
        self.flags.insert(
            name.into(),
            Flag {
                kind: Kind::Text(default.into()),
                default: default.into(),
                usage: usage.into(),
            },
        );
    }

    pub fn boolean(&mut self, name: &str, usage: &str) {
        self.flags.insert(
            name.into(),
            Flag {
                kind: Kind::Bool(false),
                default: "false".into(),
                usage: usage.into(),
            },
        );
    }

    pub fn multi(&mut self, name: &str, usage: &str) {
        self.flags.insert(
            name.into(),
            Flag {
                kind: Kind::Multi(vec![]),
                default: String::new(),
                usage: usage.into(),
            },
        );
    }

    pub fn get(&self, name: &str) -> &str {
        match &self.flags[name].kind {
            Kind::Text(v) => v,
            _ => "",
        }
    }

    pub fn on(&self, name: &str) -> bool {
        matches!(self.flags[name].kind, Kind::Bool(true))
    }

    pub fn all(&self, name: &str) -> &[String] {
        match &self.flags[name].kind {
            Kind::Multi(v) => v,
            _ => &[],
        }
    }

    pub fn usage(&self) -> String {
        let mut out = self.help.clone();
        if self.flags.is_empty() {
            return out;
        }
        out.push_str("\nOptions:\n");
        for (name, f) in &self.flags {
            let mut usage = f.usage.clone();
            let placeholder;
            if let Some(start) = usage.find('`')
                && let Some(len) = usage[start + 1..].find('`')
            {
                placeholder = usage[start + 1..start + 1 + len].to_string();
                usage = format!(
                    "{}{}{}",
                    &usage[..start],
                    placeholder,
                    &usage[start + 2 + len..]
                );
            } else {
                placeholder = match f.kind {
                    Kind::Text(_) => "string".into(),
                    Kind::Bool(_) => String::new(),
                    Kind::Multi(_) => "value".into(),
                };
            }
            let mut line = format!("  -{name}");
            if !placeholder.is_empty() {
                line.push(' ');
                line.push_str(&placeholder);
            }
            line.push_str(if line.len() <= 4 { "\t" } else { "\n    \t" });
            line.push_str(&usage.replace('\n', "\n    \t"));
            if matches!(f.kind, Kind::Text(_)) && !f.default.is_empty() {
                line.push_str(&format!(" (default {:?})", f.default));
            }
            out.push_str(&line);
            out.push('\n');
        }
        out
    }

    fn parse_one(&mut self, args: &[String]) -> Result<(usize, bool), String> {
        let mut i = 0;
        while i < args.len() {
            let a = &args[i];
            if a.len() < 2 || !a.starts_with('-') {
                return Ok((i, false));
            }
            if a == "--" {
                return Ok((i + 1, true));
            }
            let body = a.strip_prefix("--").unwrap_or(&a[1..]);
            if body.is_empty() || body.starts_with('-') || body.starts_with('=') {
                return Err(format!("bad flag syntax: {a}"));
            }
            let (name, value) = match body.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (body.to_string(), None),
            };
            if (name == "h" || name == "help") && !self.flags.contains_key(&name) {
                return Err(String::new());
            }
            let Some(f) = self.flags.get_mut(&name) else {
                return Err(format!("flag provided but not defined: -{name}"));
            };
            i += 1;
            match &mut f.kind {
                Kind::Bool(b) => {
                    *b = match value.as_deref() {
                        None | Some("true") | Some("1") | Some("t") | Some("T") | Some("TRUE")
                        | Some("True") => true,
                        Some("false") | Some("0") | Some("f") | Some("F") | Some("FALSE")
                        | Some("False") => false,
                        Some(v) => return Err(format!("invalid boolean value {v:?} for -{name}")),
                    }
                }
                kind => {
                    let v = match value {
                        Some(v) => v,
                        None if i < args.len() => {
                            i += 1;
                            args[i - 1].clone()
                        }
                        None => return Err(format!("flag needs an argument: -{name}")),
                    };
                    match kind {
                        Kind::Text(t) => *t = v,
                        Kind::Multi(m) => m.push(v),
                        Kind::Bool(_) => unreachable!(),
                    }
                }
            }
        }
        Ok((i, false))
    }

    pub fn parse(&mut self, mut args: &[String]) -> Parsed {
        let mut pos = Vec::new();
        loop {
            match self.parse_one(args) {
                Err(e) if e.is_empty() => return Parsed::Help,
                Err(e) => return Parsed::Error(e),
                Ok((n, true)) => {
                    pos.extend(args[n..].iter().cloned());
                    return Parsed::Ok(pos);
                }
                Ok((n, false)) => {
                    if n >= args.len() {
                        return Parsed::Ok(pos);
                    }
                    pos.push(args[n].clone());
                    args = &args[n + 1..];
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set() -> FlagSet {
        let mut f = FlagSet::new("Usage: x\n");
        f.text(
            "config",
            "/etc/dssr/dssr.yaml",
            "Read the configuration from this `file`.",
        );
        f.boolean("json", "Print JSON.");
        f.multi("type", "Show only this `type`.\nUse the exact value.");
        f
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn go_style() {
        let mut f = set();
        let Parsed::Ok(pos) = f.parse(&args(&[
            "wisata", "-config", "a.yaml", "--type=X", "ziarah", "--json", "--type", "Y",
        ])) else {
            panic!()
        };
        assert_eq!(pos, ["wisata", "ziarah"]);
        assert_eq!(f.get("config"), "a.yaml");
        assert!(f.on("json"));
        assert_eq!(f.all("type"), ["X", "Y"]);
        assert!(matches!(set().parse(&args(&["--nope"])), Parsed::Error(_)));
        assert!(matches!(set().parse(&args(&["-h"])), Parsed::Help));
        let Parsed::Ok(pos) = set().parse(&args(&["--", "-x"])) else {
            panic!()
        };
        assert_eq!(pos, ["-x"]);
    }

    #[test]
    fn usage_text() {
        assert_eq!(
            set().usage(),
            "Usage: x\n\nOptions:\n  -config file\n    \tRead the configuration from this file. (default \"/etc/dssr/dssr.yaml\")\n  -json\n    \tPrint JSON.\n  -type type\n    \tShow only this type.\n    \tUse the exact value.\n"
        );
    }
}
