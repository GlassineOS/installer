// SPDX-License-Identifier: GPL-2.0-or-later
//! One model of a format's structure. The validator walks a parsed TOML tree
//! against it and collects every problem with its key path (FR-002); the JSON
//! Schema is generated from the same model (FR-004), so the two never differ.

use serde_json::{Map, Value as J, json};
use toml::Value;

#[derive(Debug, Clone)]
pub enum Shape {
    Str {
        pattern: Option<&'static str>,
        values: Option<Vec<String>>,
        doc: String,
    },
    Int {
        min: Option<i64>,
        doc: String,
    },
    Bool {
        doc: String,
    },
    /// A whole number of bytes, or a string with a pattern (sizes, FR-014a).
    IntOrStr {
        pattern: &'static str,
        doc: String,
    },
    Array {
        item: Box<Shape>,
        min: usize,
        doc: String,
    },
    Table(Table),
    /// A table with keys the description chooses.
    Map {
        key_pattern: Option<&'static str>,
        keys: Option<Vec<String>>,
        value: Box<Shape>,
        doc: String,
    },
}

#[derive(Debug, Clone)]
pub struct Field {
    pub name: &'static str,
    pub shape: Shape,
    pub required: bool,
}

#[derive(Debug, Clone)]
pub struct Table {
    pub fields: Vec<Field>,
    pub doc: String,
    /// The value of the field `tag` picks the shape of the field `params`.
    pub tagged: Option<Tagged>,
    /// Groups of keys of which exactly one must be present.
    pub exactly_one: Vec<&'static [&'static str]>,
}

#[derive(Debug, Clone)]
pub struct Tagged {
    pub tag: &'static str,
    pub sub: &'static str,
    pub variants: Vec<(String, Table)>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
pub struct Problem {
    pub path: String,
    pub message: String,
}

pub fn problem(path: &str, message: impl Into<String>) -> Problem {
    Problem {
        path: if path.is_empty() { "(top)".into() } else { path.to_string() },
        message: message.into(),
    }
}

pub fn join(path: &str, key: &str) -> String {
    let simple = !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let k = if simple { key.to_string() } else { format!("\"{key}\"") };
    if path.is_empty() { k } else { format!("{path}.{k}") }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::String(_) => "a string",
        Value::Integer(_) => "an integer",
        Value::Float(_) => "a float",
        Value::Boolean(_) => "a boolean",
        Value::Datetime(_) => "a date",
        Value::Array(_) => "an array",
        Value::Table(_) => "a table",
    }
}

fn matches(pattern: &str, s: &str) -> bool {
    regex_lite::Regex::new(pattern).map(|r| r.is_match(s)).unwrap_or(false)
}

impl Shape {
    pub fn walk(&self, v: &Value, path: &str, out: &mut Vec<Problem>) {
        match (self, v) {
            (Shape::Str { pattern, values, .. }, Value::String(s)) => {
                if let Some(vals) = values
                    && !vals.iter().any(|x| x == s)
                {
                    out.push(problem(path, format!("unknown value {s:?}; expected one of: {}", vals.join(", "))));
                } else if let Some(pat) = pattern
                    && !matches(pat, s)
                {
                    out.push(problem(path, format!("{s:?} does not have the form {pat}")));
                }
            }
            (Shape::Int { min, .. }, Value::Integer(i)) => {
                if let Some(m) = min
                    && i < m
                {
                    out.push(problem(path, format!("{i} is below the least value {m}")));
                }
            }
            (Shape::Bool { .. }, Value::Boolean(_)) => {}
            (Shape::IntOrStr { .. }, Value::Integer(i)) if *i >= 0 => {}
            (Shape::IntOrStr { pattern, .. }, Value::String(s)) => {
                if !matches(pattern, s) {
                    out.push(problem(path, format!("{s:?} does not have the form {pattern}")));
                }
            }
            (Shape::Array { item, min, .. }, Value::Array(a)) => {
                if a.len() < *min {
                    out.push(problem(path, format!("needs at least {min} entr{}", if *min == 1 { "y" } else { "ies" })));
                }
                for (i, x) in a.iter().enumerate() {
                    item.walk(x, &format!("{path}[{i}]"), out);
                }
            }
            (Shape::Table(t), Value::Table(tab)) => t.walk(tab, path, out),
            (Shape::Map { key_pattern, keys, value, .. }, Value::Table(tab)) => {
                for (k, x) in tab {
                    let p = join(path, k);
                    if let Some(ks) = keys
                        && !ks.iter().any(|y| y == k)
                    {
                        out.push(problem(&p, format!("unknown key; expected one of: {}", ks.join(", "))));
                        continue;
                    }
                    if let Some(pat) = key_pattern
                        && !matches(pat, k)
                    {
                        out.push(problem(&p, format!("key {k:?} does not have the form {pat}")));
                    }
                    value.walk(x, &p, out);
                }
            }
            (s, v) => out.push(problem(path, format!("wrong type: expected {}, found {}", s.expected(), type_name(v)))),
        }
    }

    fn expected(&self) -> &'static str {
        match self {
            Shape::Str { .. } => "a string",
            Shape::Int { .. } => "an integer",
            Shape::Bool { .. } => "a boolean",
            Shape::IntOrStr { .. } => "a whole number or a string",
            Shape::Array { .. } => "an array",
            Shape::Table(_) | Shape::Map { .. } => "a table",
        }
    }

    pub fn json_schema(&self) -> J {
        match self {
            Shape::Str { pattern, values, doc } => {
                let mut m = obj(&[("type", json!("string"))], doc);
                if let Some(v) = values {
                    m.insert("enum".into(), json!(v));
                }
                if let Some(p) = pattern {
                    m.insert("pattern".into(), json!(p));
                }
                J::Object(m)
            }
            Shape::Int { min, doc } => {
                let mut m = obj(&[("type", json!("integer"))], doc);
                if let Some(x) = min {
                    m.insert("minimum".into(), json!(x));
                }
                J::Object(m)
            }
            Shape::Bool { doc } => J::Object(obj(&[("type", json!("boolean"))], doc)),
            Shape::IntOrStr { pattern, doc } => J::Object(obj(
                &[("oneOf", json!([{ "type": "integer", "minimum": 0 }, { "type": "string", "pattern": pattern }]))],
                doc,
            )),
            Shape::Array { item, min, doc } => {
                let mut m = obj(&[("type", json!("array")), ("items", item.json_schema())], doc);
                if *min > 0 {
                    m.insert("minItems".into(), json!(min));
                }
                J::Object(m)
            }
            Shape::Table(t) => t.json_schema(),
            Shape::Map { key_pattern, keys, value, doc } => {
                let mut m = obj(&[("type", json!("object")), ("additionalProperties", value.json_schema())], doc);
                let mut names = Map::new();
                if let Some(p) = key_pattern {
                    names.insert("pattern".into(), json!(p));
                }
                if let Some(k) = keys {
                    names.insert("enum".into(), json!(k));
                }
                if !names.is_empty() {
                    m.insert("propertyNames".into(), J::Object(names));
                }
                J::Object(m)
            }
        }
    }
}

fn obj(pairs: &[(&str, J)], doc: &str) -> Map<String, J> {
    let mut m = Map::new();
    if !doc.is_empty() {
        m.insert("description".into(), json!(doc));
    }
    for (k, v) in pairs {
        m.insert((*k).into(), v.clone());
    }
    m
}

impl Table {
    pub fn new(doc: impl Into<String>, fields: Vec<Field>) -> Table {
        Table {
            fields,
            doc: doc.into(),
            tagged: None,
            exactly_one: Vec::new(),
        }
    }

    pub fn one_of(mut self, keys: &'static [&'static str]) -> Table {
        self.exactly_one.push(keys);
        self
    }

    pub fn walk(&self, tab: &toml::Table, path: &str, out: &mut Vec<Problem>) {
        for f in &self.fields {
            match tab.get(f.name) {
                Some(v) => f.shape.walk(v, &join(path, f.name), out),
                None if f.required => out.push(problem(&join(path, f.name), "missing required key")),
                None => {}
            }
        }
        for group in &self.exactly_one {
            let n = group.iter().filter(|k| tab.contains_key(**k)).count();
            if n != 1 {
                let what = if n == 0 { "needs" } else { "takes only" };
                out.push(problem(path, format!("{what} exactly one of: {}", group.join(", "))));
            }
        }
        let tagged_sub = self.tagged.as_ref().map(|t| t.sub);
        for k in tab.keys() {
            if !self.fields.iter().any(|f| f.name == k) && Some(k.as_str()) != tagged_sub {
                out.push(problem(&join(path, k), "unknown key"));
            }
        }
        if let Some(t) = &self.tagged
            && let Some(Value::String(tag)) = tab.get(t.tag)
            && let Some((_, variant)) = t.variants.iter().find(|(id, _)| id == tag)
        {
            let sub = join(path, t.sub);
            match tab.get(t.sub) {
                Some(Value::Table(p)) => variant.walk(p, &sub, out),
                Some(v) => out.push(problem(&sub, format!("wrong type: expected a table, found {}", type_name(v)))),
                None if variant.fields.iter().any(|f| f.required) => variant.walk(&toml::Table::new(), &sub, out),
                None => {}
            }
        }
    }

    pub fn json_schema(&self) -> J {
        let mut props = Map::new();
        let mut req = Vec::new();
        for f in &self.fields {
            props.insert(f.name.into(), f.shape.json_schema());
            if f.required {
                req.push(json!(f.name));
            }
        }
        let mut m = obj(&[("type", json!("object"))], &self.doc);
        if let Some(t) = &self.tagged {
            props.insert(t.sub.into(), json!({ "type": "object" }));
            let all: Vec<J> = t
                .variants
                .iter()
                .map(|(id, v)| {
                    json!({
                        "if": { "properties": { t.tag: { "const": id } }, "required": [t.tag] },
                        "then": { "properties": { t.sub: v.json_schema() } }
                    })
                })
                .collect();
            m.insert("allOf".into(), J::Array(all));
        }
        for group in &self.exactly_one {
            let alts: Vec<J> = group.iter().map(|k| json!({ "required": [k] })).collect();
            let all = m.entry("allOf").or_insert_with(|| J::Array(Vec::new()));
            if let J::Array(a) = all {
                a.push(json!({ "oneOf": alts }));
            }
        }
        m.insert("properties".into(), J::Object(props));
        if !req.is_empty() {
            m.insert("required".into(), J::Array(req));
        }
        m.insert("additionalProperties".into(), json!(false));
        J::Object(m)
    }
}

pub fn f(name: &'static str, shape: Shape) -> Field {
    Field { name, shape, required: true }
}

pub fn opt(name: &'static str, shape: Shape) -> Field {
    Field { name, shape, required: false }
}

pub fn s(doc: &str) -> Shape {
    Shape::Str {
        pattern: None,
        values: None,
        doc: doc.into(),
    }
}

pub fn sp(pattern: &'static str, doc: &str) -> Shape {
    Shape::Str {
        pattern: Some(pattern),
        values: None,
        doc: doc.into(),
    }
}

pub fn se(values: &[&str], doc: &str) -> Shape {
    Shape::Str {
        pattern: None,
        values: Some(values.iter().map(|v| v.to_string()).collect()),
        doc: doc.into(),
    }
}

pub fn int(doc: &str) -> Shape {
    Shape::Int { min: None, doc: doc.into() }
}

pub fn uint(doc: &str) -> Shape {
    Shape::Int { min: Some(0), doc: doc.into() }
}

pub fn boolean(doc: &str) -> Shape {
    Shape::Bool { doc: doc.into() }
}

pub fn arr(item: Shape, doc: &str) -> Shape {
    Shape::Array {
        item: Box::new(item),
        min: 0,
        doc: doc.into(),
    }
}

pub fn arr1(item: Shape, doc: &str) -> Shape {
    Shape::Array {
        item: Box::new(item),
        min: 1,
        doc: doc.into(),
    }
}

pub fn table(t: Table) -> Shape {
    Shape::Table(t)
}
