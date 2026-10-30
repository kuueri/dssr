use serde_json::Value;

pub fn log(level: &str, msg: &str, attrs: &[(&str, Value)]) {
    let mut line = format!(
        "{{\"time\":{},\"level\":{},\"msg\":{}",
        Value::from(chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::AutoSi, false)),
        Value::from(level),
        Value::from(msg)
    );
    for (k, v) in attrs {
        line.push_str(&format!(",{}:{v}", Value::from(*k)));
    }
    line.push('}');
    println!("{line}");
}

pub fn info(msg: &str, attrs: &[(&str, Value)]) {
    log("INFO", msg, attrs);
}

pub fn warn(msg: &str, attrs: &[(&str, Value)]) {
    log("WARN", msg, attrs);
}

pub fn error(msg: &str, attrs: &[(&str, Value)]) {
    log("ERROR", msg, attrs);
}
