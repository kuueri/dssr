mod config;
mod doctor;
mod flags;
mod harvest;
mod index;
mod log;
mod oai;
mod record;
mod server;

use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt, lchown};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use config::{Config, encode_hex};
use flags::{FlagSet, Parsed};
use harvest::Harvester;
use index::{Index, SearchResult};
use oai::{encode_query, query_escape};
use server::{Problem, Server, Status};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

const USAGE: &str = "Usage:
  dssr serve   [options]
  dssr harvest [options]
  dssr cancel  [options]
  dssr suspend [options]
  dssr resume  [options]
  dssr search  [options] <query>
  dssr get     [options] <id>
  dssr status  [options]
  dssr doctor  [options]
  dssr eval    [options] <file>
  dssr update
  dssr reindex
  dssr remove <id>
  dssr revoke
  dssr uninstall
  dssr --version

Run dssr <command> --help to see the options of a command.
";

fn command_help(name: &str) -> &'static str {
    match name {
        "serve" => {
            "Usage: dssr serve [options]

Start the HTTP API, the admin socket, and the harvest schedule.
Only one dssr serve can use the index at one time.
"
        }
        "harvest" => {
            "Usage: dssr harvest [options]

Start a harvest now. dssr serve does the harvest in the background.
Run dssr status to see the result.
"
        }
        "cancel" => {
            "Usage: dssr cancel [options]

Stop the harvest that is running now.
The records that dssr already read stay in the index, and the cursor does not move, so the next harvest reads them again.
A cancelled full harvest removes no records.
The schedule does not change; to stop the next harvests too, run dssr suspend.
"
        }
        "suspend" => {
            "Usage: dssr suspend [options]

Skip the scheduled harvests and the harvest at startup until you run dssr resume.
dssr keeps this in the index, so it stays after a restart, but dssr reindex removes it.
A harvest that is running now continues; to stop it, run dssr cancel.
dssr harvest still starts a harvest when you run it.
"
        }
        "resume" => {
            "Usage: dssr resume [options]

Start the scheduled harvests again after dssr suspend.
The next scheduled harvest reads every record that changed while the schedule was suspended.
To harvest now, run dssr harvest.
"
        }
        "search" => {
            "Usage: dssr search [options] <query>

Search the index with the same logic as the HTTP API.
Every word in <query> must match.
Put a phrase in double quotes.
In a shell, put single quotes around the double quotes.
If you give a filter option more than one time, a record must match one of its values.
If you give more than one filter option, a record must match all of them.
To see the values for a filter, run dssr search <query> --json and read \"facets\".
The facet isPartOf shows the values for --collection.

Examples:
  dssr search wisata ziarah
  dssr search '\"wisata ziarah\"' --year 2020-2024 --sort newest
  dssr search ekonomi --type Thesis --type Article --language id
"
        }
        "get" => {
            "Usage: dssr get [options] <id>

Show one record as JSON.
<id> is the source ID and the record handle with a colon between them, for example repo:123456789/56.
"
        }
        "status" => {
            "Usage: dssr status [options]

Show the record count, cursor, next harvest, last harvest, and dropped fields of each source.
"
        }
        "doctor" => {
            "Usage: dssr doctor [options]

Check this dssr installation and show the result of each check.
dssr checks the version, the configuration, the service, the admin socket, the HTTP API, the index, the free disk space, and the OAI-PMH endpoint of each source.
The command fails when a check fails. A warning does not make the command fail.
Some checks need root. To do all checks, run sudo dssr doctor.
The command also works when dssr serve does not run.
"
        }
        "eval" => {
            "Usage: dssr eval [options] <file>

Measure search relevance with a list of test queries.
<file> is a YAML list.
Each item has q, an optional filter, and expect, the IDs of the records that the query must find.
dssr runs each query and looks for an expected record in the first 10 results.
The command fails when fewer than 80% of the queries find an expected record.
"
        }
        "update" => {
            "Usage: sudo dssr update

Install a newer release.
Without DSSR_VERSION, dssr installs the latest release.
With DSSR_VERSION, dssr installs that release, for example a pre-release:

  sudo DSSR_VERSION=0.1.0 dssr update

dssr does nothing if the release is not newer than the installed version.
"
        }
        "reindex" => {
            "Usage: sudo dssr reindex

Delete the index and harvest every record again.
Run it after a release that changes the index, because the index keeps the analyzer and the schema from the time dssr made it.
dssr stops, deletes the directory at index_path, and starts again.
Search results stay incomplete until the harvest of each source ends.
Run it only when every source answers OAI-PMH, because the index starts empty.
"
        }
        "remove" => {
            "Usage: sudo dssr remove <id>

Remove a source and all its records from this host.
<id> is the source ID, for example repo.
dssr stops, removes the source from /etc/dssr/dssr.yaml, deletes its records, cursor, and suspension from the index, and starts again.
If the source is not in /etc/dssr/dssr.yaml, dssr deletes only its records and its state in the index.
The command does not start while a harvest runs, because the restart stops the harvest.
You cannot undo this command. To harvest the source again, add it to /etc/dssr/dssr.yaml and restart dssr.
"
        }
        "revoke" => {
            "Usage: sudo dssr revoke

Revoke the client token, make a new one, and show the new token one time.
dssr keeps only the hash of the token in /etc/dssr/dssr.yaml, then restarts, so the old token stops working.
Give the new token to the client service, which sends it in the header Authorization: Bearer <token>.
"
        }
        "uninstall" => {
            "Usage: sudo dssr uninstall

Stop dssr and remove it from this host.
The command removes the service, the binary, the configuration, the index, and the dssr user and group.
You cannot undo this command.
"
        }
        _ => "",
    }
}

const UNIT_FILE: &str = "/etc/systemd/system/dssr.service";
const UNIT_DROP_IN: &str = "/etc/systemd/system/dssr.service.d";
const WANTS_FILE: &str = "/etc/systemd/system/multi-user.target.wants/dssr.service";
const BINARY_FILE: &str = "/usr/local/bin/dssr";
const DOC_DIR: &str = "/usr/local/share/doc/dssr";
const CONFIG_DIR: &str = "/etc/dssr";
const CONFIG_FILE: &str = "/etc/dssr/dssr.yaml";
const STATE_DIR: &str = "/var/lib/dssr";
const RUNTIME_DIR: &str = "/run/dssr";

const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/kuueri/dssr/releases/latest";
const INSTALL_SCRIPT_URL: &str =
    "https://raw.githubusercontent.com/kuueri/dssr/refs/tags/{}/install.sh";

fn build_version() -> &'static str {
    option_env!("DSSR_BUILD_VERSION").unwrap_or("")
}

fn version() -> &'static str {
    match build_version() {
        "" => "devel",
        v => v,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprint!("{USAGE}");
        std::process::exit(2);
    }
    let rest = &args[2..];
    let result = match args[1].as_str() {
        "--version" | "-version" => {
            println!("{}", version());
            Ok(())
        }
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(())
        }
        "serve" => serve(rest),
        "harvest" => harvest_cmd(rest),
        "cancel" => cancel_cmd(rest),
        "suspend" => suspend_cmd("suspend", rest),
        "resume" => suspend_cmd("resume", rest),
        "search" => search(rest),
        "get" => get(rest),
        "status" => status(rest),
        "doctor" => doctor::doctor(rest),
        "eval" => eval(rest),
        "update" => update(rest),
        "reindex" => reindex(rest),
        "remove" => remove(rest),
        "revoke" => revoke(rest),
        "uninstall" => uninstall(rest),
        _ => {
            eprint!("{USAGE}");
            std::process::exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("dssr: {e}");
        std::process::exit(1);
    }
}

fn parse(fs: &mut FlagSet, args: &[String]) -> Vec<String> {
    match fs.parse(args) {
        Parsed::Ok(pos) => pos,
        Parsed::Help => {
            eprint!("{}", fs.usage());
            std::process::exit(0);
        }
        Parsed::Error(e) => {
            eprintln!("{e}");
            eprint!("{}", fs.usage());
            std::process::exit(2);
        }
    }
}

fn new_flags(name: &str) -> FlagSet {
    let mut fs = FlagSet::new(command_help(name));
    fs.text(
        "config",
        CONFIG_FILE,
        "Read the configuration from this `file`.",
    );
    fs
}

fn no_args(name: &str, args: &[String]) -> Result<()> {
    let mut fs = FlagSet::new(command_help(name));
    if !parse(&mut fs, args).is_empty() {
        return Err(format!("{name} takes no arguments").into());
    }
    Ok(())
}

fn is_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

fn serve(args: &[String]) -> Result<()> {
    let mut fs = new_flags("serve");
    parse(&mut fs, args);
    let cfg = Config::load(fs.get("config"))?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(serve_async(cfg))
}

fn listen_unix(path: &str) -> Result<tokio::net::UnixListener> {
    if std::os::unix::net::UnixStream::connect(path).is_ok() {
        return Err(format!("{path}: another dssr serve is running").into());
    }
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let ln = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))?;
    Ok(ln)
}

async fn serve_async(cfg: Config) -> Result<()> {
    let admin_ln = listen_unix(&cfg.admin_socket)?;
    let api_ln = tokio::net::TcpListener::bind(&cfg.listen).await?;
    let idx = Arc::new(
        Index::open(Path::new(&cfg.index_path))
            .map_err(|e| format!("open index {}: {e}", cfg.index_path))?,
    );

    let agent = format!("dssr/{}", version());
    let h = Harvester::new(idx.clone(), cfg.sources.clone(), &agent);
    let srv = Server::new(idx, h.clone(), &cfg, version());
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let graceful = |mut rx: tokio::sync::watch::Receiver<bool>| async move {
        let _ = rx.wait_for(|v| *v).await;
    };
    let api = axum::serve(api_ln, server::api(srv.clone()))
        .with_graceful_shutdown(graceful(stop_rx.clone()));
    let admin = axum::serve(admin_ln, server::admin(srv)).with_graceful_shutdown(graceful(stop_rx));
    let mut api = tokio::spawn(async move { api.await });
    let mut admin = tokio::spawn(async move { admin.await });

    h.schedule(cfg.location).map_err(|e| e.to_string())?;
    for s in &cfg.sources {
        match h.start_scheduled(&s.id) {
            Err(harvest::Error::Suspended) => log::info(
                "startup harvest skipped",
                &[
                    ("source", json!(s.id)),
                    ("err", json!(harvest::Error::Suspended.to_string())),
                ],
            ),
            Err(e) => return Err(e.to_string().into()),
            Ok(()) => {}
        }
    }
    log::info(
        "serving",
        &[
            ("version", json!(version())),
            ("listen", json!(cfg.listen)),
            ("admin_socket", json!(cfg.admin_socket)),
            ("index", json!(cfg.index_path)),
        ],
    );

    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut result: Result<()> = Ok(());
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
        r = &mut api => result = r.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.to_string())).map_err(Into::into),
        r = &mut admin => result = r.map_err(|e| e.to_string()).and_then(|r| r.map_err(|e| e.to_string())).map_err(Into::into),
    }
    log::info("shutting down", &[]);
    let _ = stop_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(10), async {
        let _ = api.await;
        let _ = admin.await;
    })
    .await;
    h.stop().await;
    result
}

fn call(cfg_path: &str, method: &str, path: &str) -> Result<Vec<u8>> {
    let cfg = match Config::load(cfg_path) {
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::PermissionDenied) =>
        {
            return Err(format!("{e}; run as root, or add your user to the dssr group with sudo usermod -aG dssr $USER and log in again").into());
        }
        r => r?,
    };
    let mut conn = std::os::unix::net::UnixStream::connect(&cfg.admin_socket)
        .map_err(|e| format!("is dssr serve running? {e}"))?;
    write!(
        conn,
        "{method} {path} HTTP/1.0\r\nHost: dssr\r\nContent-Length: 0\r\n\r\n"
    )?;
    let mut raw = Vec::new();
    conn.read_to_end(&mut raw)?;
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("malformed response from dssr serve")?;
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let body = raw[split + 4..].to_vec();
    let status_line = head.lines().next().unwrap_or("");
    let code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    if code >= 400 {
        if let Ok(p) = serde_json::from_slice::<Problem>(&body)
            && !p.detail.is_empty()
        {
            return Err(p.detail.into());
        }
        let status = status_line.split_once(' ').map(|s| s.1).unwrap_or("");
        return Err(format!("{method} {path}: {status}").into());
    }
    Ok(body)
}

fn harvest_cmd(args: &[String]) -> Result<()> {
    let mut fs = new_flags("harvest");
    fs.text(
        "source",
        "",
        "Harvest only the source with this `ID`.\nWithout this option, dssr harvests all sources.",
    );
    fs.boolean(
        "full",
        "Read all records of the source, then remove the records that the source no longer lists.\nWithout this option, dssr reads only the records that changed after the last harvest.\ndssr removes no records if the harvest sees fewer than 90% of the indexed records.",
    );
    parse(&mut fs, args);
    let mut q = Vec::new();
    if !fs.get("source").is_empty() {
        q.push(("source", fs.get("source")));
    }
    if fs.on("full") {
        q.push(("full", "true"));
    }
    let body = call(
        fs.get("config"),
        "POST",
        &format!("/harvest?{}", encode_query(&q)),
    )?;
    #[derive(Deserialize)]
    struct Res {
        started: Vec<String>,
        full: bool,
    }
    let res: Res = serde_json::from_slice(&body)?;
    let kind = if res.full { "full" } else { "incremental" };
    println!(
        "{kind} harvest started for {}; follow it with dssr status",
        res.started.join(", ")
    );
    Ok(())
}

fn source_query(fs: &mut FlagSet, args: &[String]) -> String {
    fs.text("source", "", "Do this only for the source with this `ID`.\nWithout this option, dssr does it for all sources.");
    parse(fs, args);
    match fs.get("source") {
        "" => String::new(),
        s => format!("?{}", encode_query(&[("source", s)])),
    }
}

fn cancel_cmd(args: &[String]) -> Result<()> {
    let mut fs = new_flags("cancel");
    let q = source_query(&mut fs, args);
    let body = call(fs.get("config"), "POST", &format!("/cancel{q}"))?;
    #[derive(Deserialize)]
    struct Res {
        cancelled: Vec<String>,
    }
    let res: Res = serde_json::from_slice(&body)?;
    if res.cancelled.is_empty() {
        println!("no harvest was running");
    } else {
        println!(
            "cancelled the harvest of {}; the cursor stays where it was",
            res.cancelled.join(", ")
        );
    }
    Ok(())
}

fn suspend_cmd(name: &str, args: &[String]) -> Result<()> {
    let mut fs = new_flags(name);
    let q = source_query(&mut fs, args);
    let body = call(fs.get("config"), "POST", &format!("/{name}{q}"))?;
    #[derive(Deserialize)]
    struct Res {
        sources: Vec<String>,
    }
    let res: Res = serde_json::from_slice(&body)?;
    if name == "suspend" {
        println!(
            "suspended the schedule of {}; run dssr resume to start it again",
            res.sources.join(", ")
        );
    } else {
        println!("resumed the schedule of {}", res.sources.join(", "));
    }
    Ok(())
}

fn search(args: &[String]) -> Result<()> {
    let mut fs = new_flags("search");
    fs.multi("type", "Show only records of this `type`, for example Thesis.\nUse the exact value, with the same uppercase and lowercase letters.");
    fs.multi(
        "collection",
        "Show only records in this `collection` or community.\nUse the exact name.",
    );
    fs.multi(
        "language",
        "Show only records in this `language`, for example id or en.\nUse the exact value.",
    );
    fs.text("year", "", "Show only records issued in this `year` or range of years, for example 2020 or 2020-2024.\nThe range includes the first and the last year.");
    fs.text("sort", "relevance", "Sort the results in this `order`: relevance or newest.\nWith newest, the latest issue date comes first.");
    fs.text("page", "1", "Show this `page` of results. The first page is 1.\ndssr shows only the first 1000 results.");
    fs.text(
        "limit",
        "10",
        "Show this `number` of results on each page, from 1 to 50.",
    );
    fs.boolean("json", "Print the full result as JSON, with the facets.");
    let q = parse(&mut fs, args).join(" ");

    let mut v: Vec<(&str, &str)> = vec![("q", &q)];
    for key in ["type", "collection", "language"] {
        v.extend(fs.all(key).iter().map(|x| (key, x.as_str())));
    }
    v.extend([
        ("sort", fs.get("sort")),
        ("page", fs.get("page")),
        ("limit", fs.get("limit")),
    ]);
    if !fs.get("year").is_empty() {
        v.push(("year", fs.get("year")));
    }
    let body = call(
        fs.get("config"),
        "GET",
        &format!("/search?{}", encode_query(&v)),
    )?;
    if fs.on("json") {
        std::io::stdout().write_all(&body)?;
        return Ok(());
    }
    let res: SearchResult = serde_json::from_slice(&body)?;
    println!("{} results", res.total);
    for (i, h) in res.results.iter().enumerate() {
        print!(
            "{:3}. {}",
            (res.page - 1) * res.limit + i + 1,
            h.record.title
        );
        if !h.record.issued.is_empty() {
            print!(" ({})", h.record.issued);
        }
        println!("\n     {}  score {:.2}", h.record.id, h.score);
        if !h.record.creator.is_empty() {
            println!("     {}", h.record.creator.join("; "));
        }
    }
    Ok(())
}

fn get(args: &[String]) -> Result<()> {
    let mut fs = new_flags("get");
    fs.boolean(
        "json",
        "Print the record as compact JSON. Without this option, dssr indents the JSON.",
    );
    let pos = parse(&mut fs, args);
    if pos.len() != 1 {
        return Err("get needs exactly one record id, for example repo:123456789/56".into());
    }
    let Some((source, handle)) = pos[0].split_once(':') else {
        return Err(format!("{:?} is not a record id like repo:123456789/56", pos[0]).into());
    };
    let body = call(
        fs.get("config"),
        "GET",
        &format!(
            "/records/{}/{handle}",
            query_escape(source).replace('+', "%20")
        ),
    )?;
    if fs.on("json") {
        std::io::stdout().write_all(&body)?;
        return Ok(());
    }
    let r: record::Record = serde_json::from_slice(&body)?;
    println!("{}", serde_json::to_string_pretty(&r)?);
    Ok(())
}

fn local_time(t: &chrono::DateTime<chrono::FixedOffset>) -> String {
    t.with_timezone(&chrono::Local)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

fn status(args: &[String]) -> Result<()> {
    let mut fs = new_flags("status");
    fs.boolean("json", "Print the status as JSON.");
    parse(&mut fs, args);
    let body = call(fs.get("config"), "GET", "/status")?;
    if fs.on("json") {
        std::io::stdout().write_all(&body)?;
        return Ok(());
    }
    let st: Status = serde_json::from_slice(&body)?;
    println!("dssr {}, up since {}", st.version, local_time(&st.started));
    for s in &st.sources {
        println!("\n{} ({})", s.id, s.name);
        println!("  documents  {}", s.docs);
        println!(
            "  cursor     {}",
            if s.cursor.is_empty() {
                "none"
            } else {
                &s.cursor
            }
        );
        if !s.suspended.is_empty() {
            let since = chrono::DateTime::parse_from_rfc3339(&s.suspended)
                .map(|t| local_time(&t))
                .unwrap_or_else(|_| s.suspended.clone());
            println!("  suspended  since {since}; run dssr resume to start the schedule again");
        } else if let Some(next) = &s.next {
            println!("  next run   {}", local_time(next));
        }
        println!("  running    {}", s.running);
        if let Some(l) = &s.last {
            let kind = if l.full { "full" } else { "incremental" };
            println!(
                "  last run   {kind}, {} to {}, upserted {}, deleted {}, swept {}",
                local_time(&l.started),
                local_time(&l.finished),
                l.upserted,
                l.deleted,
                l.swept
            );
            if !l.note.is_empty() {
                println!("  note       {}", l.note);
            }
            if !l.error.is_empty() {
                println!("  error      {}", l.error);
            }
        }
        if !s.dropped.is_empty() {
            let mut keys: Vec<_> = s.dropped.iter().collect();
            keys.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
            println!("  dropped fields since start");
            for (k, n) in keys {
                println!("    {k:<40} {n}");
            }
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EvalCase {
    q: String,
    #[serde(default)]
    filter: EvalFilter,
    #[serde(default)]
    expect: Vec<String>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct EvalFilter {
    #[serde(default)]
    r#type: Vec<String>,
    #[serde(default)]
    year: String,
    #[serde(default)]
    collection: Vec<String>,
    #[serde(default)]
    language: Vec<String>,
}

fn eval(args: &[String]) -> Result<()> {
    let mut fs = new_flags("eval");
    let pos = parse(&mut fs, args);
    if pos.len() != 1 {
        return Err("eval needs exactly one YAML file".into());
    }
    let text = std::fs::read_to_string(&pos[0])?;
    let cases: Vec<EvalCase> =
        serde_norway::from_str(&text).map_err(|e| format!("{}: {e}", pos[0]))?;
    let mut hits = 0;
    for c in &cases {
        let mut v: Vec<(&str, &str)> = vec![("q", &c.q), ("limit", "10")];
        for (key, values) in [
            ("type", &c.filter.r#type),
            ("collection", &c.filter.collection),
            ("language", &c.filter.language),
        ] {
            v.extend(values.iter().map(|x| (key, x.as_str())));
        }
        if !c.filter.year.is_empty() {
            v.push(("year", &c.filter.year));
        }
        let body = call(
            fs.get("config"),
            "GET",
            &format!("/search?{}", encode_query(&v)),
        )
        .map_err(|e| format!("{:?}: {e}", c.q))?;
        let res: SearchResult = serde_json::from_slice(&body)?;
        match res
            .results
            .iter()
            .position(|h| c.expect.contains(&h.record.id))
        {
            Some(i) => {
                hits += 1;
                println!("HIT  {:2}  {}", i + 1, c.q);
            }
            None => println!(
                "MISS  -  {} (total {}, expect {})",
                c.q,
                res.total,
                c.expect.join(", ")
            ),
        }
    }
    if cases.is_empty() {
        return Err(format!("no queries in {}", pos[0]).into());
    }
    let pct = 100.0 * hits as f64 / cases.len() as f64;
    println!("\nhit@10: {hits}/{} ({pct:.1}%)", cases.len());
    if pct < 80.0 {
        return Err("hit@10 is below 80%".into());
    }
    Ok(())
}

fn is_version(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()))
}

fn newer(a: &str, b: &str) -> bool {
    let nums = |s: &str| -> Vec<u64> {
        let mut v: Vec<u64> = s.split('.').map(|p| p.parse().unwrap_or(0)).collect();
        v.resize(3, 0);
        v
    };
    nums(a) > nums(b)
}

fn fetch(url: &str) -> Result<Vec<u8>> {
    oai::install_tls();
    let c = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .user_agent(format!("dssr/{}", version()))
        .build()?;
    let res = c.get(url).send()?;
    if res.status() != reqwest::StatusCode::OK {
        return Err(format!("GET {url}: {}", res.status()).into());
    }
    let mut out = Vec::new();
    res.take(1 << 20).read_to_end(&mut out)?;
    Ok(out)
}

fn latest_release(api: &str) -> Result<String> {
    let b = fetch(api).map_err(|e| format!("find the latest release: {e}"))?;
    #[derive(Deserialize)]
    struct R {
        tag_name: String,
    }
    let r: R = serde_json::from_slice(&b).map_err(|e| format!("find the latest release: {e}"))?;
    if !is_version(&r.tag_name) {
        return Err(format!(
            "the latest release tag {:?} is not a version like 0.1.0",
            r.tag_name
        )
        .into());
    }
    Ok(r.tag_name)
}

fn update(args: &[String]) -> Result<()> {
    no_args("update", args)?;
    if !is_root() {
        return Err("run as root: sudo dssr update".into());
    }
    let current = version();
    println!("Current version: {current}");
    let tag = match std::env::var("DSSR_VERSION").unwrap_or_default() {
        t if t.is_empty() => {
            println!("Checking for updates to latest version...");
            latest_release(LATEST_RELEASE_URL).map_err(|e| {
                format!(
                    "{e}; to install a pre-release, run: sudo DSSR_VERSION=<version> dssr update"
                )
            })?
        }
        t if !is_version(&t) => {
            return Err(format!("DSSR_VERSION {t:?} is not a version like 0.1.0").into());
        }
        t => {
            println!("Checking for updates to version {t}...");
            t
        }
    };
    if !newer(&tag, build_version()) {
        println!("dssr is up to date ({current})");
        return Ok(());
    }
    let script = fetch(&INSTALL_SCRIPT_URL.replace("{}", &tag))?;
    println!("Updating dssr {current} to {tag}...");
    let mut child = Command::new("bash")
        .env("DSSR_VERSION", &tag)
        .stdin(Stdio::piped())
        .spawn()?;
    child.stdin.take().ok_or("bash stdin")?.write_all(&script)?;
    if !child.wait()?.success() {
        return Err(format!("install.sh {tag}: exit status {}", child.wait()?).into());
    }
    Ok(())
}

fn systemctl() -> Option<PathBuf> {
    std::env::var_os("PATH")?
        .to_str()?
        .split(':')
        .map(|d| Path::new(d).join("systemctl"))
        .find(|p| p.is_file())
}

fn run_output(cmd: &Path, args: &[&str]) -> std::result::Result<(), String> {
    match Command::new(cmd).args(args).output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(format!(
            "{}: {}",
            o.status,
            String::from_utf8_lossy(&[o.stdout, o.stderr].concat()).trim()
        )),
        Err(e) => Err(e.to_string()),
    }
}

fn is_active(systemctl: &Path) -> bool {
    Command::new(systemctl)
        .args(["is-active", "--quiet", "dssr.service"])
        .status()
        .is_ok_and(|s| s.success())
}

fn is_index(path: &Path) -> bool {
    ["state.json", "tantivy/meta.json", "index_meta.json"]
        .iter()
        .any(|f| path.join(f).exists())
}

fn uninstall_paths(path: &str) -> (String, String, Result<()>) {
    let (mut index_path, mut admin_socket) = (
        format!("{STATE_DIR}/index"),
        format!("{RUNTIME_DIR}/admin.sock"),
    );
    let text = match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (index_path, admin_socket, Ok(()));
        }
        Err(e) => return (index_path, admin_socket, Err(e.into())),
        Ok(t) => t,
    };
    #[derive(Deserialize)]
    struct Paths {
        #[serde(default)]
        index_path: String,
        #[serde(default)]
        admin_socket: String,
    }
    match serde_norway::from_str::<Paths>(&text) {
        Err(e) => (index_path, admin_socket, Err(format!("{path}: {e}").into())),
        Ok(p) => {
            if !p.index_path.is_empty() {
                index_path = p.index_path;
            }
            if !p.admin_socket.is_empty() {
                admin_socket = p.admin_socket;
            }
            (index_path, admin_socket, Ok(()))
        }
    }
}

fn clean(path: &str) -> String {
    let abs = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|l| *l != "..") {
                    parts.pop();
                } else if !abs {
                    parts.push("..");
                }
            }
            p => parts.push(p),
        }
    }
    let joined = parts.join("/");
    match (abs, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".into(),
        (false, false) => joined,
    }
}

fn check_removable(path: &str) -> Result<()> {
    let c = clean(path);
    let refused = [
        "/bin",
        "/boot",
        "/dev",
        "/etc",
        "/home",
        "/lib",
        "/lib64",
        "/media",
        "/mnt",
        "/opt",
        "/proc",
        "/root",
        "/run",
        "/sbin",
        "/srv",
        "/sys",
        "/tmp",
        "/usr",
        "/var",
        "/etc/systemd",
        "/etc/systemd/system",
        "/usr/local",
        "/usr/local/bin",
        "/usr/local/share",
        "/usr/share",
        "/var/lib",
        "/var/log",
        "/var/cache",
        "/var/spool",
        "/var/tmp",
    ];
    if !c.starts_with('/') || c.matches('/').count() < 2 || refused.contains(&c.as_str()) {
        return Err(format!("refusing to remove {path:?}").into());
    }
    Ok(())
}

fn mailbox_paths(login_defs: &str, name: &str) -> Vec<String> {
    let mut dirs: Vec<String> = std::fs::read_to_string(login_defs)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            (f.len() >= 2 && f[0] == "MAIL_DIR" && f[1].starts_with('/')).then(|| f[1].to_string())
        })
        .collect();
    if dirs.is_empty() {
        dirs = vec!["/var/mail".into(), "/var/spool/mail".into()];
    }
    dirs.iter()
        .map(|d| format!("{}/{name}", d.trim_end_matches('/')))
        .collect()
}

fn account_exists(file: &str, name: &str) -> bool {
    std::fs::read_to_string(file)
        .unwrap_or_default()
        .lines()
        .any(|l| l.split(':').next() == Some(name))
}

fn socket_in_use(path: &str) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

fn uninstall(args: &[String]) -> Result<()> {
    no_args("uninstall", args)?;
    if !is_root() {
        return Err("run as root: sudo dssr uninstall".into());
    }
    let (index_path, admin_socket, err) = uninstall_paths(CONFIG_FILE);
    if let Err(e) = err {
        eprintln!("dssr: warning: {e}; removing the default paths");
    }

    let sc = systemctl();
    match &sc {
        Some(sc) => {
            if let Err(e) = run_output(sc, &["disable", "--now", "dssr.service"])
                && is_active(sc)
            {
                return Err(format!("stop dssr.service: {e}").into());
            }
            if is_active(sc) {
                return Err("dssr.service is still active; stop it and run uninstall again".into());
            }
        }
        None => eprintln!("dssr: warning: systemctl not found; the service was not stopped"),
    }

    let mut errs: Vec<String> = Vec::new();
    let remove = |errs: &mut Vec<String>, path: &str| {
        if let Err(e) = std::fs::remove_file(path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            errs.push(format!("remove {path}: {e}"));
        }
    };
    let remove_tree = |errs: &mut Vec<String>, path: &str| {
        let c = clean(path);
        if let Err(e) = check_removable(&c) {
            errs.push(e.to_string());
            return;
        }
        if let Err(e) = std::fs::remove_dir_all(&c)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            errs.push(format!("remove {c}: {e}"));
        }
    };

    if std::fs::symlink_metadata(&admin_socket).is_ok_and(|m| m.file_type().is_socket())
        && socket_in_use(&admin_socket)
    {
        return Err(format!("{admin_socket} is in use; stop dssr and run uninstall again").into());
    }

    remove(&mut errs, WANTS_FILE);
    remove(&mut errs, UNIT_FILE);
    remove_tree(&mut errs, UNIT_DROP_IN);
    if let Some(sc) = &sc {
        if let Err(e) = run_output(sc, &["daemon-reload"]) {
            eprintln!("dssr: warning: systemctl daemon-reload: {e}");
        }
        let _ = run_output(sc, &["reset-failed", "dssr.service"]);
    }

    if is_index(Path::new(&index_path)) {
        remove_tree(&mut errs, &index_path);
    }
    remove_tree(&mut errs, STATE_DIR);
    if let Ok(m) = std::fs::symlink_metadata(&admin_socket) {
        if m.file_type().is_socket() {
            remove(&mut errs, &admin_socket);
        } else {
            errs.push(format!("{admin_socket} is not a socket, leaving it"));
        }
    }
    remove_tree(&mut errs, RUNTIME_DIR);
    remove_tree(&mut errs, CONFIG_DIR);
    remove_tree(&mut errs, DOC_DIR);

    let mut user_removed = true;
    if account_exists("/etc/passwd", "dssr")
        && let Err(e) = run_output(Path::new("userdel"), &["dssr"])
    {
        user_removed = false;
        errs.push(format!("userdel dssr: {e}"));
    }
    if user_removed {
        for path in mailbox_paths("/etc/login.defs", "dssr") {
            let Ok(m) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !m.file_type().is_file() {
                errs.push(format!("{path} is not a regular mailbox file, leaving it"));
                continue;
            }
            remove(&mut errs, &path);
        }
    }
    if account_exists("/etc/group", "dssr")
        && let Err(e) = run_output(Path::new("groupdel"), &["dssr"])
    {
        errs.push(format!("groupdel dssr: {e}"));
    }
    if errs.is_empty() {
        remove(&mut errs, BINARY_FILE);
    }

    for path in [
        WANTS_FILE,
        UNIT_FILE,
        UNIT_DROP_IN,
        BINARY_FILE,
        DOC_DIR,
        CONFIG_FILE,
        CONFIG_DIR,
        STATE_DIR,
        RUNTIME_DIR,
        &index_path,
        &admin_socket,
    ] {
        if std::fs::symlink_metadata(path).is_ok() {
            errs.push(format!("{path} still exists"));
        }
    }
    if account_exists("/etc/passwd", "dssr") {
        errs.push("user dssr still exists".into());
    }
    if account_exists("/etc/group", "dssr") {
        errs.push("group dssr still exists".into());
    }
    if !errs.is_empty() {
        return Err(errs.join("\n").into());
    }
    println!("dssr uninstalled");
    println!(
        "journald keeps past dssr entries in the system journal; it cannot delete one unit's history"
    );
    Ok(())
}

fn reindex(args: &[String]) -> Result<()> {
    no_args("reindex", args)?;
    if !is_root() {
        return Err("run as root: sudo dssr reindex".into());
    }
    let (index_path, admin_socket, err) = uninstall_paths(CONFIG_FILE);
    err?;
    let index_path = clean(&index_path);
    check_removable(&index_path)?;
    if !is_index(Path::new(&index_path)) {
        return Err(format!("{index_path} is not a dssr index").into());
    }
    let sc = systemctl().ok_or(
        "systemctl not found; stop dssr, delete the index directory, and start dssr by hand",
    )?;
    run_output(&sc, &["stop", "dssr.service"]).map_err(|e| format!("stop dssr.service: {e}"))?;
    if is_active(&sc) {
        return Err("dssr.service is still active; stop it and run reindex again".into());
    }
    if socket_in_use(&admin_socket) {
        return Err(format!("{admin_socket} is in use; stop dssr and run reindex again").into());
    }
    std::fs::remove_dir_all(&index_path)
        .map_err(|e| format!("remove {index_path}: {e}; dssr is stopped"))?;
    run_output(&sc, &["start", "dssr.service"]).map_err(|e| format!("start dssr.service: {e}"))?;
    println!("removed {index_path}; dssr is harvesting every record again");
    println!("run dssr status to follow the harvest");
    Ok(())
}

fn chown_tree(path: &Path, uid: u32, gid: u32) -> Result<()> {
    lchown(path, Some(uid), Some(gid))?;
    if std::fs::symlink_metadata(path)?.file_type().is_dir() {
        for entry in std::fs::read_dir(path)? {
            chown_tree(&entry?.path(), uid, gid)?;
        }
    }
    Ok(())
}

fn remove(args: &[String]) -> Result<()> {
    let mut fs = new_flags("remove");
    let pos = parse(&mut fs, args);
    if pos.len() != 1 {
        return Err("remove needs exactly one source id, for example repo".into());
    }
    let id = &pos[0];
    if !config::valid_source_id(id) {
        return Err(format!("{id:?} is not a source id").into());
    }
    if !is_root() {
        return Err("run as root: sudo dssr remove <id>".into());
    }
    let cfg_path = fs.get("config");
    let cfg = Config::load(cfg_path)?;
    if let Ok(body) = call(cfg_path, "GET", "/status")
        && let Ok(st) = serde_json::from_slice::<Status>(&body)
    {
        let ids: Vec<&str> = st
            .sources
            .iter()
            .filter(|s| s.running)
            .map(|s| s.id.as_str())
            .collect();
        if !ids.is_empty() {
            return Err(format!(
                "a harvest is running for {}; wait until it ends or run dssr cancel, then run dssr remove again",
                ids.join(", ")
            )
            .into());
        }
    }
    let sc = systemctl().ok_or("systemctl not found; stop dssr and run dssr remove again")?;
    run_output(&sc, &["stop", "dssr.service"]).map_err(|e| format!("stop dssr.service: {e}"))?;
    let restart = |msg: String| -> Error {
        match run_output(&sc, &["start", "dssr.service"]) {
            Ok(()) => msg.into(),
            Err(e) => format!("{msg}; dssr is stopped: {e}").into(),
        }
    };
    if is_active(&sc) {
        return Err(restart(
            "dssr.service is still active; stop it and run dssr remove again".into(),
        ));
    }
    if socket_in_use(&cfg.admin_socket) {
        return Err(restart(format!(
            "{} is in use; stop dssr and run dssr remove again",
            cfg.admin_socket
        )));
    }
    let index_path = Path::new(&cfg.index_path);
    let in_config = cfg.sources.iter().any(|s| s.id == *id);
    if !index_path.exists() && !in_config {
        return Err(restart(format!(
            "unknown source {id}: it is not in {cfg_path} or in the index"
        )));
    }
    let owner = std::fs::metadata(index_path)
        .or_else(|_| std::fs::metadata(index_path.parent().unwrap_or(Path::new("."))))
        .map(|m| (m.uid(), m.gid()))
        .map_err(|e| restart(format!("read the owner of {}: {e}", index_path.display())))?;
    let result: Result<(bool, usize)> = (|| {
        let idx = if index_path.exists() {
            Some(Index::open(index_path)?)
        } else {
            None
        };
        let known = in_config
            || idx.as_ref().is_some_and(|x| {
                x.count(id).ok().is_some_and(|n| n > 0)
                    || ["cursor", "gen", "suspended"]
                        .iter()
                        .any(|k| !x.meta(&format!("{k}:{id}")).is_empty())
            });
        if !known {
            return Err(
                format!("unknown source {id}: it is not in {cfg_path} or in the index").into(),
            );
        }
        let config_removed = if in_config {
            let text = std::fs::read_to_string(cfg_path)?;
            let (out, removed) = remove_source(&text, id)?;
            if !removed {
                return Err(format!("cannot remove source {id:?} from {cfg_path}").into());
            }
            replace_config(cfg_path, &out)?;
            true
        } else {
            false
        };
        let n = match idx {
            Some(idx) => {
                let n = idx.purge(id)?;
                drop(idx);
                n
            }
            None => 0,
        };
        if index_path.exists() {
            chown_tree(index_path, owner.0, owner.1)?;
        }
        Ok((config_removed, n))
    })();
    let start = run_output(&sc, &["start", "dssr.service"]);
    match (result, start) {
        (Ok((config_removed, n)), Ok(())) => {
            println!("removed source {id}: deleted {n} records and its cursor from the index");
            if config_removed {
                println!("removed {id} from {cfg_path}");
            }
            Ok(())
        }
        (Ok(_), Err(e)) => Err(format!("start dssr.service: {e}").into()),
        (Err(e), Ok(())) => Err(e),
        (Err(e), Err(start)) => Err(format!("{e}; dssr is stopped: {start}").into()),
    }
}

fn revoke(args: &[String]) -> Result<()> {
    no_args("revoke", args)?;
    if !is_root() {
        return Err("run as root: sudo dssr revoke".into());
    }
    let mut secret = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut secret)?;
    let tok = format!("sk-{}", encode_hex(&secret));
    let hash = encode_hex(&Sha256::digest(tok.as_bytes()));

    let text = std::fs::read_to_string(CONFIG_FILE)?;
    let (out, replaced) =
        set_client_token(&text, "default", &hash).map_err(|e| format!("{CONFIG_FILE}: {e}"))?;
    replace_config(CONFIG_FILE, &out)?;

    println!("New token, shown one time, store it now:\n{tok}");
    if replaced {
        println!("The previous token no longer works.");
    }
    run_output(Path::new("systemctl"), &["try-restart", "dssr.service"])
        .map_err(|e| format!("the token is saved, but restarting dssr.service failed: {e}"))?;
    Ok(())
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn strip_comment(v: &str) -> &str {
    v.split(" #").next().unwrap_or("").trim()
}

fn unquote(v: &str) -> &str {
    v.trim_matches(|c| c == '"' || c == '\'')
}

fn set_client_token(text: &str, name: &str, hash: &str) -> Result<(String, bool)> {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let item = |ind: &str| {
        vec![
            format!("{ind}- name: {name}"),
            format!("{ind}  token_sha256: {hash}"),
        ]
    };
    let mut replaced = false;
    match lines.iter().position(|l| l.starts_with("clients:")) {
        None => {
            lines.push("clients:".into());
            lines.extend(item("  "));
        }
        Some(at) => {
            let value = strip_comment(&lines[at]["clients:".len()..]);
            if !matches!(value, "" | "~" | "null" | "[]") {
                return Err("clients is not a list".into());
            }
            lines[at] = "clients:".into();
            let mut end = at;
            let mut dash_indent: Option<String> = None;
            let mut current_name = String::new();
            let mut i = at + 1;
            while i < lines.len() {
                let l = lines[i].clone();
                let t = l.trim_start();
                if t.is_empty() || t.starts_with('#') {
                    i += 1;
                    continue;
                }
                if indent(&l) == 0 && !t.starts_with("- ") {
                    break;
                }
                end = i;
                let mut field = t;
                if let Some(rest) = t.strip_prefix("- ") {
                    dash_indent.get_or_insert_with(|| " ".repeat(indent(&l)));
                    current_name.clear();
                    field = rest.trim_start();
                }
                if let Some(v) = field.strip_prefix("name:") {
                    current_name = unquote(strip_comment(v)).to_string();
                }
                if field.starts_with("token_sha256:") && current_name == name {
                    let key_at = l.len() - field.len();
                    lines[i] = format!("{}token_sha256: {hash}", &l[..key_at]);
                    replaced = true;
                }
                i += 1;
            }
            if !replaced {
                let ind = dash_indent.unwrap_or_else(|| "  ".into());
                let at = end + 1;
                for (k, l) in item(&ind).into_iter().enumerate() {
                    lines.insert(at + k, l);
                }
            }
        }
    }
    let mut out = lines.join("\n");
    out.push('\n');
    #[derive(Deserialize)]
    struct Clients {
        #[serde(default)]
        clients: Vec<config::Client>,
    }
    let parsed: Clients = serde_norway::from_str(&out)?;
    if !parsed
        .clients
        .iter()
        .any(|c| c.name == name && c.token_sha256 == hash)
    {
        return Err("cannot write the token into clients".into());
    }
    Ok((out, replaced))
}

fn remove_source(text: &str, id: &str) -> Result<(String, bool)> {
    fn parse_ids(text: &str) -> Result<Vec<String>> {
        #[derive(Deserialize)]
        struct Sources {
            #[serde(default)]
            sources: Option<Vec<config::Source>>,
        }
        Ok(serde_norway::from_str::<Sources>(text)?
            .sources
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.id)
            .collect())
    }

    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let Some(at) = lines.iter().position(|l| l.starts_with("sources:")) else {
        return Ok((text.to_string(), false));
    };
    let value = strip_comment(&lines[at]["sources:".len()..]);
    if !matches!(value, "" | "~" | "null" | "[]") {
        return Err("sources is not a block list".into());
    }
    let mut items: Vec<(usize, usize, String)> = Vec::new();
    let mut current: Option<(usize, usize, String)> = None;
    let mut i = at + 1;
    while i < lines.len() {
        let l = lines[i].clone();
        let t = l.trim_start();
        if t.is_empty() || t.starts_with('#') {
            i += 1;
            continue;
        }
        if indent(&l) == 0 && !t.starts_with("- ") {
            break;
        }
        if let Some(field) = t.strip_prefix("- ") {
            if let Some(item) = current.take() {
                items.push(item);
            }
            let field = field.trim_start();
            let id_value = field
                .strip_prefix("id:")
                .map(|v| unquote(strip_comment(v)).to_string())
                .unwrap_or_default();
            current = Some((i, i, id_value));
        } else if let Some((_, end, id_value)) = current.as_mut() {
            *end = i;
            if let Some(v) = t.strip_prefix("id:") {
                *id_value = unquote(strip_comment(v)).to_string();
            }
        }
        i += 1;
    }
    if let Some(item) = current {
        items.push(item);
    }
    let Some((start, end, _)) = items.iter().find(|(_, _, item_id)| item_id == id) else {
        return Ok((text.to_string(), false));
    };

    let old = parse_ids(text)?;
    let expected: Vec<String> = old.iter().filter(|old_id| *old_id != id).cloned().collect();
    lines.drain(*start..=*end);
    let mut out = lines.join("\n");
    if text.ends_with('\n') {
        out.push('\n');
    }
    let new = parse_ids(&out)?;
    if new != expected {
        return Err(format!("cannot remove source {id:?} from sources").into());
    }
    Ok((out, true))
}

fn replace_config(path: &str, data: &str) -> Result<()> {
    let meta = std::fs::metadata(path)?;
    let dir = Path::new(path).parent().unwrap_or(Path::new("."));
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".dssr.yaml.{}{nanos}", std::process::id()));
    let result = (|| -> Result<()> {
        std::fs::write(&tmp, data)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(meta.mode() & 0o777))?;
        std::os::unix::fs::chown(&tmp, Some(meta.uid()), Some(meta.gid()))?;
        Config::load(tmp.to_str().ok_or("path")?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN_CONFIG: &str = "listen: 127.0.0.1:8080
index_path: /var/lib/dssr/index
admin_socket: /run/dssr/admin.sock
timezone: Asia/Jakarta

clients:
  - name: default
    token_sha256: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa

# Add each OAI-PMH source, then run: sudo systemctl restart dssr
# sources:
#   - id: repo
";

    const SOURCES_CONFIG: &str = "listen: 127.0.0.1:8080
index_path: /var/lib/dssr/index
admin_socket: /run/dssr/admin.sock

sources:
  - id: repo
    name: Repository
    oai_url: https://example.org/oai/request
    schedule: \"0 3 * * 1\"
  # the catalog
  - name: Catalog
    id: \"slims\"
    oai_url: https://example.org/oai2.php
    schedule: \"0 4 * * *\"

# trailing note
";

    fn source_ids(s: &str) -> Vec<String> {
        #[derive(Deserialize)]
        struct S {
            #[serde(default)]
            sources: Option<Vec<config::Source>>,
        }
        serde_norway::from_str::<S>(s)
            .unwrap()
            .sources
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.id)
            .collect()
    }

    #[test]
    fn remove_present() {
        let (out, removed) = remove_source(SOURCES_CONFIG, "repo").unwrap();
        assert!(removed);
        assert_eq!(source_ids(&out), ["slims"]);
        for keep in [
            "  # the catalog\n",
            "# trailing note\n",
            "listen: 127.0.0.1:8080\n",
        ] {
            assert!(out.contains(keep), "lost {keep:?}:\n{out}");
        }
        let (out, removed) = remove_source(SOURCES_CONFIG, "slims").unwrap();
        assert!(removed);
        assert_eq!(source_ids(&out), ["repo"]);
        assert!(out.contains("# trailing note\n"));
        let (out, _) = remove_source(&out, "repo").unwrap();
        assert!(source_ids(&out).is_empty());
    }

    #[test]
    fn remove_absent() {
        assert_eq!(
            remove_source(SOURCES_CONFIG, "nope").unwrap(),
            (SOURCES_CONFIG.to_string(), false)
        );
        assert_eq!(
            remove_source(TOKEN_CONFIG, "repo").unwrap(),
            (TOKEN_CONFIG.to_string(), false)
        );
        assert!(remove_source("sources: [{id: repo}]\n", "repo").is_err());
    }

    fn clients_of(s: &str) -> Vec<config::Client> {
        #[derive(Deserialize)]
        struct C {
            clients: Vec<config::Client>,
        }
        serde_norway::from_str::<C>(s).unwrap().clients
    }

    #[test]
    fn set_token() {
        let b = "b".repeat(64);
        let (out, replaced) = set_client_token(TOKEN_CONFIG, "default", &b).unwrap();
        assert!(replaced);
        let c = clients_of(&out);
        assert_eq!((c.len(), c[0].token_sha256.as_str()), (1, b.as_str()));
        for want in ["\n  - name: default\n    token_sha256: ", "#   - id: repo"] {
            assert!(out.contains(want), "output lacks {want:?}:\n{out}");
        }

        let c64 = "c".repeat(64);
        let (out, replaced) = set_client_token(&out, "digilib", &c64).unwrap();
        assert!(!replaced);
        let c = clients_of(&out);
        assert_eq!(
            (c.len(), c[1].name.as_str(), c[1].token_sha256.as_str()),
            (2, "digilib", c64.as_str())
        );

        for input in [
            "listen: 127.0.0.1:8080\n",
            "listen: 127.0.0.1:8080\nclients:\n",
        ] {
            let (out, _) = set_client_token(input, "default", &b).unwrap();
            let c = clients_of(&out);
            assert_eq!(
                (c.len(), c[0].token_sha256.as_str()),
                (1, b.as_str()),
                "{input:?}"
            );
        }
        assert!(set_client_token("clients: nope\n", "default", &b).is_err());
    }

    #[test]
    fn replace() {
        let (_t, dir) = index::tests::temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dssr.yaml");
        let p = path.to_str().unwrap();
        std::fs::write(&path, TOKEN_CONFIG).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert!(replace_config(p, &format!("{TOKEN_CONFIG}bogus: 1\n")).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), TOKEN_CONFIG);

        let next = TOKEN_CONFIG.replacen("name: default", "name: library", 1);
        replace_config(p, &next).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o640);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), next);
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".dssr.yaml."))
            .collect();
        assert!(left.is_empty());
    }

    #[test]
    fn chown_tree_nested() {
        let (_t, dir) = index::tests::temp_dir();
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("a/b/f"), "x").unwrap();
        std::os::unix::fs::symlink("/nonexistent", dir.join("a/link")).unwrap();
        let m = std::fs::metadata(&dir).unwrap();
        chown_tree(&dir, m.uid(), m.gid()).unwrap();
        assert!(chown_tree(&dir.join("missing"), m.uid(), m.gid()).is_err());
    }

    #[test]
    fn paths() {
        let (_t, dir) = index::tests::temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dssr.yaml");
        let p = path.to_str().unwrap();
        let (index, socket, err) = uninstall_paths(p);
        assert!(err.is_ok());
        assert_eq!(
            (index.as_str(), socket.as_str()),
            ("/var/lib/dssr/index", "/run/dssr/admin.sock")
        );
        std::fs::write(
            &path,
            "index_path: /srv/dssr/index\nadmin_socket: /srv/dssr/admin.sock\nsources: []\n",
        )
        .unwrap();
        let (index, socket, err) = uninstall_paths(p);
        assert!(err.is_ok());
        assert_eq!(
            (index.as_str(), socket.as_str()),
            ("/srv/dssr/index", "/srv/dssr/admin.sock")
        );
        std::fs::write(&path, "index_path: [\n").unwrap();
        assert!(uninstall_paths(p).2.is_err());
    }

    #[test]
    fn removable() {
        for p in [
            "/",
            "/etc",
            "/var/lib",
            "/usr/local",
            "/etc/systemd/system",
            "relative",
            "",
            "/var/lib/../../etc",
        ] {
            assert!(check_removable(p).is_err(), "{p:?}");
        }
        for p in [
            "/var/lib/dssr",
            "/var/lib/dssr/index",
            "/srv/dssr/index",
            "/run/dssr",
        ] {
            assert!(check_removable(p).is_ok(), "{p:?}");
        }
    }

    #[test]
    fn mailboxes() {
        let (_t, dir) = index::tests::temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("login.defs");
        let p = path.to_str().unwrap();
        std::fs::write(&path, "# mail\nMAIL_DIR /srv/mail\n").unwrap();
        assert_eq!(mailbox_paths(p, "dssr"), ["/srv/mail/dssr"]);
        std::fs::write(&path, "MAIL_DIR mail\n").unwrap();
        assert_eq!(
            mailbox_paths(p, "dssr"),
            ["/var/mail/dssr", "/var/spool/mail/dssr"]
        );
        assert_eq!(
            mailbox_paths(dir.join("missing").to_str().unwrap(), "dssr"),
            ["/var/mail/dssr", "/var/spool/mail/dssr"]
        );
    }

    #[test]
    fn versions() {
        for (a, b, want) in [
            ("0.2.0", "0.1.9", true),
            ("0.10.0", "0.9.0", true),
            ("1.0.0", "", true),
            ("0.1.0", "0.1.0", false),
            ("0.1.0", "0.2.0", false),
        ] {
            assert_eq!(newer(a, b), want, "newer({a:?}, {b:?})");
        }
        assert!(is_version("0.2.0") && !is_version("v0.2.0") && !is_version("0.2"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn latest() {
        let ok = oai::tests::serve(Arc::new(|_: &str, _: &str| {
            (200, vec![], r#"{"tag_name":"0.2.0"}"#.into())
        }))
        .await;
        let prefixed = oai::tests::serve(Arc::new(|_: &str, _: &str| {
            (200, vec![], r#"{"tag_name":"v0.2.0"}"#.into())
        }))
        .await;
        let missing =
            oai::tests::serve(Arc::new(|_: &str, _: &str| (404, vec![], String::new()))).await;
        let r = tokio::task::spawn_blocking(move || {
            (
                latest_release(&ok).ok(),
                latest_release(&prefixed).is_err(),
                latest_release(&missing).is_err(),
            )
        })
        .await
        .unwrap();
        assert_eq!(r, (Some("0.2.0".to_string()), true, true));
    }
}
