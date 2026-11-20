//! A form for any command, built from its parameters' schema: a checkbox, a number, a choice, a
//! body picked from a list, a point typed as "x, y, z". Commands without a form of their own open
//! this instead of the command box (nobody has to type a command call), the selected bodies filled
//! in. Drawn by [`crate::cam_editor`] (its `Target::Command`), the same window as the CAM forms.
use serde_json::Value;

/// What a parameter is edited as.
#[derive(Clone, Debug, PartialEq)]
pub enum AutoKind {
    Bool,
    Integer,
    Number,
    /// A length: a number, or an expression such as "w / 2".
    Length,
    Text,
    /// One of a list of words.
    Choice(Vec<String>),
    /// A body of the design, picked from a list.
    Body,
    /// Several bodies, ticked in a list.
    Bodies,
    /// A point or vector typed as "x, y, z".
    Point,
    /// Nothing the form can edit (a nested structure): set by the command's own tools.
    Other,
}

/// One parameter of the form.
#[derive(Clone, Debug)]
pub struct AutoField {
    pub key: String,
    pub label: String,
    pub kind: AutoKind,
    pub required: bool,
    pub help: String,
}

/// Optional parameters the form leaves out: plumbing for scripts, not choices a person makes.
const HIDDEN: &[&str] = &["participants", "body_names", "live", "preview", "tolerance"];

/// The command's input schema, as JSON.
fn schema(id: &str) -> Option<Value> {
    let spec = solidcraft_engine::find_command(id)?;
    let contract = spec.contract?;
    Some(contract.input_schema().as_value().clone())
}

/// Whether starting `id` with no parameters can only fail: it needs some.
pub fn needs_form(id: &str) -> bool {
    schema(id).is_some_and(|s| s.get("required").and_then(Value::as_array).is_some_and(|a| !a.is_empty()))
}

/// The form's fields for `id`, the required ones first; `None` without a schema.
pub fn fields(id: &str) -> Option<Vec<AutoField>> {
    let root = schema(id)?;
    let defs = root.get("$defs");
    let props = root.get("properties")?.as_object()?;
    let required: Vec<&str> = root.get("required").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let mut out = Vec::new();
    for (key, p) in props {
        let req = required.contains(&key.as_str());
        if !req && HIDDEN.contains(&key.as_str()) {
            continue;
        }
        let (kind, help) = kind_of(key, p, defs);
        if kind == AutoKind::Other && !req {
            continue;
        }
        out.push(AutoField { key: key.clone(), label: label(key), kind, required: req, help: first_line(&help) });
    }
    out.sort_by_key(|f| !f.required);
    Some(out)
}

fn first_line(s: &str) -> String {
    s.split("\n\n").next().unwrap_or("").replace('\n', " ")
}

/// "patch_holes" → "Patch holes".
fn label(key: &str) -> String {
    let spaced = key.replace('_', " ");
    let mut c = spaced.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// `v` with its `$ref`s followed into `$defs`.
fn resolve<'a>(mut v: &'a Value, defs: Option<&'a Value>) -> &'a Value {
    for _ in 0..8 {
        let next = v.get("$ref").and_then(Value::as_str).and_then(|r| r.strip_prefix("#/$defs/")).and_then(|n| defs.and_then(|d| d.get(n)));
        match next {
            Some(d) => v = d,
            None => break,
        }
    }
    v
}

fn types(p: &Value) -> Vec<&str> {
    match p.get("type") {
        Some(Value::String(s)) => vec![s.as_str()],
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).filter(|t| *t != "null").collect(),
        _ => Vec::new(),
    }
}

fn is_bodies(key: &str) -> bool {
    key == "bodies" || key.ends_with("_bodies")
}

fn is_body(key: &str) -> bool {
    key == "body" || key.ends_with("_body")
}

fn kind_of(key: &str, p: &Value, defs: Option<&Value>) -> (AutoKind, String) {
    let p = resolve(p, defs);
    let help = p.get("description").and_then(Value::as_str).unwrap_or("").to_string();
    let alts: Vec<&Value> = ["anyOf", "oneOf"]
        .iter()
        .filter_map(|k| p.get(*k).and_then(Value::as_array))
        .flatten()
        .map(|a| resolve(a, defs))
        .filter(|a| types(a) != ["null"] && a.get("type").and_then(Value::as_str) != Some("null"))
        .collect();
    if !alts.is_empty() {
        let consts: Vec<String> = alts
            .iter()
            .filter_map(|a| {
                a.get("const").and_then(Value::as_str).map(str::to_string).or_else(|| {
                    a.get("enum").and_then(Value::as_array).filter(|e| e.len() == 1).and_then(|e| e.first()).and_then(Value::as_str).map(str::to_string)
                })
            })
            .collect();
        if consts.len() == alts.len() {
            return (AutoKind::Choice(consts), help);
        }
        if let [one] = alts.as_slice() {
            let (k, h) = kind_of(key, one, defs);
            return (k, if help.is_empty() { h } else { help });
        }
        let ts: Vec<&str> = alts.iter().flat_map(|a| types(a)).collect();
        if is_bodies(key) && ts.contains(&"string") {
            return (AutoKind::Bodies, help);
        }
        if ts.contains(&"number") && ts.contains(&"string") && ts.len() == 2 {
            return (AutoKind::Length, help);
        }
        return (AutoKind::Other, help);
    }
    if let Some(e) = p.get("enum").and_then(Value::as_array) {
        let c: Vec<String> = e.iter().filter_map(Value::as_str).map(str::to_string).collect();
        if !c.is_empty() {
            return (AutoKind::Choice(c), help);
        }
    }
    let ts = types(p);
    let kind = match ts.as_slice() {
        ["boolean"] => AutoKind::Bool,
        ["integer"] => AutoKind::Integer,
        ["number"] => AutoKind::Number,
        ["string"] if is_body(key) => AutoKind::Body,
        ["string"] => AutoKind::Text,
        ["number", "string"] | ["string", "number"] => AutoKind::Length,
        ["array"] => {
            let item = p.get("items").map(|i| resolve(i, defs));
            let it = item.map(types).unwrap_or_default();
            if is_bodies(key) && it == ["string"] {
                AutoKind::Bodies
            } else if it == ["number"] && p.get("maxItems").and_then(Value::as_u64).is_some_and(|n| n <= 3) {
                AutoKind::Point
            } else {
                AutoKind::Other
            }
        }
        _ => AutoKind::Other,
    };
    (kind, help)
}
