// SPDX-License-Identifier: GPL-2.0-or-later
//! The installer in the browser. Pages get versioned JSON records, never Rust
//! types: `inspect()` gives a `Connection`, whose `facts` is a facts snapshot
//! (`facts_format`) and whose `reboot()` restarts the device in the same
//! fastboot session, the only command besides getvar it can send; `check(facts)`
//! a report (`report_format`), `plan(facts, input)` a plan (`plan_format`),
//! `description(id)` the source of a shipped description (TOML), for the
//! texts the screens show.
//! A failure is a record too: `{"error": KIND, "message": TEXT}`, KIND one of
//! no-device, several-devices, busy, permission, usb, not-fresh, timeout,
//! device, descriptions, facts, input, closed.
//!
//! The records are built by plain functions (tested natively); the browser
//! entry points only add WebUSB around them.

use glassine_installer::facts::Snapshot;
use glassine_installer::format1::Description;
use glassine_installer::plan::{self, Input};
use glassine_installer::validate::{self, Loaded};
use glassine_installer::{recognize, report};
use serde_json::{Value, json};

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));

/// The shipped description copies that load; others are left out, as the
/// command line does.
pub fn descriptions() -> Vec<Description> {
    EMBEDDED
        .iter()
        .filter_map(|(_, t)| match validate::load(t) {
            Loaded::Valid(d) => Some(*d),
            _ => None,
        })
        .collect()
}

/// The source (TOML) of the shipped description `id`, when it loads: the
/// screens take its texts from there, so the site needs no copy of its own.
pub fn description_source(id: &str) -> Option<&'static str> {
    EMBEDDED.iter().map(|(_, t)| *t).find(|t| match validate::load(t) {
        Loaded::Valid(d) => d.id == id,
        _ => false,
    })
}

pub fn error(kind: &str, message: impl std::fmt::Display) -> String {
    json!({ "error": kind, "message": message.to_string() }).to_string()
}

fn snapshot(facts: &str) -> Result<Snapshot, String> {
    Snapshot::parse(facts).map_err(|e| error("facts", e))
}

/// The check report for a facts snapshot.
pub fn check_record(facts: &str) -> Result<String, String> {
    let ds = descriptions();
    let refs: Vec<&Description> = ds.iter().collect();
    let f = snapshot(facts)?;
    let rec = recognize::recognize(&refs, &f);
    let p = plan::plan(&refs, &f, &rec, &Input::default());
    Ok(serde_json::to_string(&report::report(&refs, &f, &rec, Some(&p))).unwrap())
}

/// The plan's input from a page: `{"choices": {K: V}, "hardware": {H: bool},
/// "developer": bool, "job": ID, "free_bytes": N}`, every key optional.
pub fn input(json: &str) -> Result<Input, String> {
    let bad = |what: &str| {
        error(
            "input",
            format!("{what}; expected {{\"choices\": {{}}, \"hardware\": {{}}, \"developer\": false}}"),
        )
    };
    let v: Value = if json.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(json).map_err(|e| bad(&e.to_string()))?
    };
    let o = v.as_object().ok_or_else(|| bad("not an object"))?;
    let mut i = Input::default();
    for (k, v) in o {
        match k.as_str() {
            "choices" => {
                for (c, x) in v.as_object().ok_or_else(|| bad("choices is not an object"))? {
                    i.choices.insert(c.clone(), x.as_str().ok_or_else(|| bad("a choice is not text"))?.to_string());
                }
            }
            "hardware" => {
                for (h, x) in v.as_object().ok_or_else(|| bad("hardware is not an object"))? {
                    i.hardware
                        .insert(h.clone(), x.as_bool().ok_or_else(|| bad("a hardware answer is not true or false"))?);
                }
            }
            "developer" => i.developer = v.as_bool().ok_or_else(|| bad("developer is not true or false"))?,
            "job" => i.job = Some(v.as_str().ok_or_else(|| bad("job is not text"))?.to_string()),
            "free_bytes" => i.free_bytes = Some(v.as_u64().ok_or_else(|| bad("free_bytes is not a number"))?),
            other => return Err(bad(&format!("unknown key {other:?}"))),
        }
    }
    Ok(i)
}

/// The dry-run plan for a facts snapshot and the person's answers.
pub fn plan_record(facts: &str, answers: &str) -> Result<String, String> {
    let ds = descriptions();
    let refs: Vec<&Description> = ds.iter().collect();
    let f = snapshot(facts)?;
    let i = input(answers)?;
    let rec = recognize::recognize(&refs, &f);
    Ok(serde_json::to_string(&plan::plan(&refs, &f, &rec, &i)).unwrap())
}

#[cfg(target_arch = "wasm32")]
mod web {
    use super::*;
    use glassine_installer::fastboot;
    use glassine_installer::inspect::{self, Host};
    use glassine_installer_usb as usb;
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::{future_to_promise, js_sys};

    fn js(r: Result<String, String>) -> Result<String, JsValue> {
        r.map_err(|e| JsValue::from_str(&e))
    }

    /// Asks the browser for a fastboot device (call it from a click: WebUSB
    /// needs a user gesture), inspects it and resolves with a [`Connection`]:
    /// the facts snapshot, and the fastboot session kept open for one
    /// restart. Rejects with an error record.
    #[wasm_bindgen]
    pub async fn inspect() -> Result<Connection, JsValue> {
        let ds = descriptions();
        let refs: Vec<&Description> = ds.iter().collect();
        let (t, ids) = usb::request().await.map_err(|e| JsValue::from_str(&error(e.kind(), &e)))?;
        let host = Host {
            platform: "browser".into(),
            connections: vec!["usb-web".into()],
        };
        let (got, session) = inspect::inspect_open(&refs, t, &ids, &host)
            .await
            .map_err(|e| JsValue::from_str(&error(e.kind(), &e)))?;
        Ok(Connection {
            facts: serde_json::to_string(&got.snapshot).unwrap(),
            session: Rc::new(RefCell::new(Some(session))),
        })
    }

    /// One inspected device and its fastboot session, still open. The Note
    /// Air's boot loader answers only the session of a fresh start: once this
    /// one is closed, it can't be restarted from the page any more, and stays
    /// in its USB mode until its power button is held. So the page keeps the
    /// session until the person restarts the device (or it goes away).
    #[wasm_bindgen]
    pub struct Connection {
        facts: String,
        session: Rc<RefCell<Option<fastboot::Session<usb::UsbTransport>>>>,
    }

    #[wasm_bindgen]
    impl Connection {
        /// The facts snapshot of the inspect.
        #[wasm_bindgen(getter)]
        pub fn facts(&self) -> String {
            self.facts.clone()
        }

        /// Whether the session is still open: neither restarted nor closed.
        #[wasm_bindgen(getter)]
        pub fn open(&self) -> bool {
            self.session.borrow().is_some()
        }

        /// Sends `reboot` in this session, once: the session is taken at the
        /// call, so a second call (a double click, then the page closing)
        /// sends nothing and rejects. Resolves when the device answered OKAY;
        /// rejects with an error record (`closed` when there is no session).
        pub fn reboot(&self) -> js_sys::Promise {
            let taken = self.session.borrow_mut().take();
            future_to_promise(async move {
                let mut s = taken.ok_or_else(|| JsValue::from_str(&error("closed", "the device's session is closed")))?;
                s.reboot().await.map_err(|e| JsValue::from_str(&error("device", e)))?;
                Ok(JsValue::TRUE)
            })
        }

        /// Ends the session without sending anything (the device was
        /// unplugged, or another one is chosen). Releases the interface.
        pub fn close(&self) {
            self.session.borrow_mut().take();
        }
    }

    /// The check report record for a facts snapshot.
    #[wasm_bindgen]
    pub fn check(facts: &str) -> Result<String, JsValue> {
        js(check_record(facts))
    }

    /// The dry-run plan record for a facts snapshot and the answers (see `input`).
    #[wasm_bindgen]
    pub fn plan(facts: &str, answers: &str) -> Result<String, JsValue> {
        js(plan_record(facts, answers))
    }

    /// The source (TOML) of the shipped description `id`, or `undefined`.
    #[wasm_bindgen]
    pub fn description(id: &str) -> Option<String> {
        description_source(id).map(str::to_string)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{}/../core/tests/fixtures/facts/{name}.json", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    #[test]
    fn records_are_versioned_json() {
        let f = fixture("noteair-stock-fastboot");
        let r: Value = serde_json::from_str(&check_record(&f).unwrap()).unwrap();
        assert_eq!(r["report_format"], 1);
        let p: Value = serde_json::from_str(&plan_record(&f, r#"{"hardware": {"emergency-cable": true}, "developer": true}"#).unwrap()).unwrap();
        assert_eq!(p["plan_format"], 1);
        assert_eq!(p["status"], "ready");
        let p: Value = serde_json::from_str(&plan_record(&f, "").unwrap()).unwrap();
        assert_eq!(p["status"], "needs-answers");
    }

    #[test]
    fn failures_are_records() {
        let e: Value = serde_json::from_str(&check_record("{").unwrap_err()).unwrap();
        assert_eq!(e["error"], "facts");
        let f = fixture("noteair-stock-fastboot");
        for bad in ["[]", r#"{"choices": 1}"#, r#"{"nope": 1}"#, r#"{"hardware": {"x": "yes"}}"#] {
            let e: Value = serde_json::from_str(&plan_record(&f, bad).unwrap_err()).unwrap();
            assert_eq!(e["error"], "input", "{bad}");
        }
    }

    #[test]
    fn the_shipped_descriptions_load() {
        assert!(descriptions().len() >= 2);
    }

    #[test]
    fn a_description_source_by_id() {
        for d in descriptions() {
            let src = description_source(&d.id).unwrap();
            assert!(matches!(validate::load(src), Loaded::Valid(x) if x.id == d.id));
        }
        assert_eq!(description_source("no-such-device"), None);
        assert_eq!(description_source(""), None);
    }
}
