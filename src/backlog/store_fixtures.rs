//! The real store's answers (test only), read from `tests/fixtures/store/answers.json`. The file is written by
//! `scripts/store-fixtures/generate.sh` from Retaskable.com's `shared/*.ts` run in memory, so a fixture here is
//! what the store gives and a request in it is one the store's own argument checks took. Fake stores in the
//! tests answer with these instead of hand-written JSON, which drifts. See `tests/fixtures/store/README.md`.
use serde_json::{Value, json};

const FILE: &str = include_str!("../../tests/fixtures/store/answers.json");

fn file() -> Value {
    serde_json::from_str(FILE).expect("tests/fixtures/store/answers.json is JSON")
}

/// One recorded op: the request the store took and the answer it gave (`value`), or the code it refused with.
pub struct Case {
    pub op: String,
    pub args: Value,
    pub value: Value,
    pub error: Option<String>,
}

/// The case `name` (the file lists them all).
pub fn case(name: &str) -> Case {
    let file = file();
    let c = file["cases"]
        .get(name)
        .unwrap_or_else(|| panic!("no store fixture {name}: run scripts/store-fixtures/generate.sh"));
    Case {
        op: c["op"].as_str().unwrap().to_string(),
        args: c["args"].clone(),
        value: c.get("value").cloned().unwrap_or(Value::Null),
        error: c.get("error").and_then(Value::as_str).map(String::from),
    }
}

/// What the store sends over HTTP for `name`: `{ "value": … }`.
pub fn answer(name: &str) -> Value {
    json!({ "value": case(name).value })
}

/// The answer's value alone.
pub fn value(name: &str) -> Value {
    case(name).value
}

/// A limit the store enforces, found by probing it (`merge_result_reason`: the most UTF-16 units of a `reason`).
pub fn limit(name: &str) -> usize {
    file()["limits"][name]
        .as_u64()
        .unwrap_or_else(|| panic!("no store limit {name}")) as usize
}

/// The names of all cases.
pub fn names() -> Vec<String> {
    file()["cases"].as_object().unwrap().keys().cloned().collect()
}

/// Checks that what the client `sent` is what the store took in `name`: every field it sends is one the fixture has, with
/// the same value (objects field by field). Fields the fixture has and the client leaves out are optional ones.
pub fn assert_sent(name: &str, sent: &Value) {
    fn subset(path: &str, sent: &Value, took: &Value) {
        match (sent, took) {
            (Value::Object(s), Value::Object(t)) => {
                for (k, v) in s {
                    let Some(tv) = t.get(k) else {
                        panic!("{path}.{k}: the client sends a field the store took no fixture of: {v}")
                    };
                    subset(&format!("{path}.{k}"), v, tv);
                }
            }
            _ => assert_eq!(sent, took, "{path}"),
        }
    }
    subset(name, sent, &case(name).args);
}

#[cfg(test)]
mod tests;
