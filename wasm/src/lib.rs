// SPDX-License-Identifier: GPL-2.0-or-later
//! The installer in the browser. Pages get versioned JSON records, never Rust
//! types: `inspect()` gives a `Connection`, whose `facts` is a facts snapshot
//! (`facts_format`) and whose `reboot()` restarts the device in the same
//! fastboot session, the only command besides getvar it can send; `check(facts)`
//! a report (`report_format`), `plan(facts, input)` a plan (`plan_format`),
//! `description(id)` the source of a shipped description (TOML), for the
//! texts the screens show, `supplied_check(device, file, bytes, reported)` the
//! check of a file the person picked for a mode (`supplied_check_format`),
//! made in the page: the file's bytes never leave it.
//! A failure is a record too: `{"error": KIND, "message": TEXT}`, KIND one of
//! no-device, several-devices, busy, permission, usb, not-fresh, timeout,
//! device, descriptions, facts, input, closed.
//!
//! The records are built by plain functions (tested natively); the browser
//! entry points only add WebUSB around them.

use glassine_installer::emergency_read::{Done, EmergencyError, Failure, Restart, Scope};
use glassine_installer::facts::Snapshot;
use glassine_installer::format1::Description;
use glassine_installer::plan::{self, Input};
use glassine_installer::release::Release;
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
/// "developer": bool, "job": ID, "free_bytes": N, "release": MANIFEST}`, every
/// key optional; MANIFEST is the installer release's manifest (002 FR-019a).
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
            "release" => i.release = Some(Release::parse(&v.to_string()).map_err(|e| bad(&e))?),
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

/// The check of a file the person supplied (004 FR-004) as a record:
/// `reported` is what the device reported in the mode as a JSON object of
/// the route's keys (`{"chip": HEX, "pk_hash": HEX}`), or empty before it is
/// there. Only reads the bytes.
pub fn supplied_record(device: &str, file: &str, bytes: &[u8], reported: &str) -> Result<String, String> {
    let ds = descriptions();
    let d = ds
        .iter()
        .find(|d| d.id == device)
        .ok_or_else(|| error("input", format!("no description {device:?}")))?;
    let rep: Option<std::collections::BTreeMap<String, String>> = if reported.trim().is_empty() {
        None
    } else {
        Some(serde_json::from_str(reported).map_err(|e| error("input", format!("reported: {e}; expected {{\"KEY\": \"VALUE\"}}")))?)
    };
    let c = glassine_installer::supplied::check(d, file, bytes, rep.as_ref()).map_err(|e| error("input", e))?;
    Ok(serde_json::to_string(&c).unwrap())
}

/// The programmer the person picked for the emergency read, checked again
/// here as the command line does before it opens a device (004 FR-004): a
/// known file goes on; one that fits but isn't known only with
/// `accept_unknown` (the person ticked the warning); any other never does.
/// The page's own check isn't trusted. A refusal is an error record
/// (`not-sendable`, or `input` for a question the description can't answer).
pub fn emergency_programmer(device: &str, file: &str, bytes: &[u8], accept_unknown: bool) -> Result<(), String> {
    let ds = descriptions();
    let d = ds
        .iter()
        .find(|d| d.id == device)
        .ok_or_else(|| error("input", format!("no description {device:?}")))?;
    let c = glassine_installer::supplied::check(d, file, bytes, None).map_err(|e| error("input", e))?;
    match c.verdict.sendable() {
        Some(false) => Ok(()),
        Some(true) if accept_unknown => Ok(()),
        Some(true) => Err(error(
            "not-sendable",
            "the file isn't a known one and its warning wasn't confirmed; nothing was sent",
        )),
        None => Err(error("not-sendable", "the file doesn't fit this device; nothing was sent")),
    }
}

/// What an emergency read reads after the GPT: `whole-disk` (the plan's
/// backup, 001 FR-009), `boot-areas`, or `partitions:NAME,NAME`. Something is
/// always stored: the browser route has no table-only run.
pub fn emergency_scope(scope: &str) -> Result<Scope, String> {
    match scope {
        "whole-disk" => Ok(Scope::WholeDisk),
        "boot-areas" => Ok(Scope::BootAreas),
        _ => match scope.strip_prefix("partitions:") {
            Some(l) if l.split(',').all(|n| !n.is_empty()) => Ok(Scope::Partitions(l.split(',').map(str::to_string).collect())),
            _ => Err(error(
                "input",
                format!("scope {scope:?}; expected whole-disk, boot-areas or partitions:NAME,NAME"),
            )),
        },
    }
}

fn restart_record(r: &Restart) -> (&'static str, Option<String>) {
    match r {
        Restart::Done => ("done", None),
        Restart::Failed(w) => ("failed", Some(w.clone())),
        Restart::NotPossible => ("by-hand", None),
    }
}

/// The record of an emergency read (`emergency_read_format` 1). `outcome`:
/// `done`; `refused` (the boot ROM didn't take the file, or it didn't answer
/// its first request: it doesn't work on this device, 004 FR-006, and
/// nothing was read); `nothing-sent` (the boot ROM didn't answer, or reports
/// a chip this installer doesn't know); `failed` (anything after the
/// programmer answered: the images that are complete and those that aren't
/// are in `manifest`). `restart`: `done`, `failed` (sent, not confirmed;
/// `restart_why`) or `by-hand` (nothing answers that could restart it).
/// No serial or other value unique to the unit.
pub fn emergency_record(r: &Result<Done, Failure>) -> String {
    let manifest = |m: &Option<String>| m.as_deref().and_then(|t| serde_json::from_str::<Value>(t).ok());
    match r {
        Ok(d) => {
            let (restart, why) = restart_record(&d.restart);
            json!({
                "emergency_read_format": 1,
                "outcome": "done",
                "chip": d.chip,
                "images": d.images.iter().map(|(rec, start, file)| json!({
                    "name": rec.disk.name,
                    "physical_partition": rec.disk.index,
                    "start_sector": start,
                    "size": rec.disk.size,
                    "sha256": rec.sha256_hex(),
                    "file": file,
                })).collect::<Vec<_>>(),
                "manifest": manifest(&d.manifest),
                "restart": restart,
                "restart_why": why,
            })
            .to_string()
        }
        Err(f) => {
            let (restart, why) = restart_record(&f.restart);
            let (outcome, kind) = match &f.error {
                EmergencyError::Refused(_) => ("refused", "refused"),
                EmergencyError::NotWorking(_) => ("refused", "not-working"),
                EmergencyError::BootRom(_) => ("nothing-sent", "boot-rom"),
                EmergencyError::UnknownChip(_) => ("nothing-sent", "unknown-chip"),
                EmergencyError::Firehose(_) => ("failed", "programmer"),
                EmergencyError::Storage(_) => ("failed", "storage"),
                EmergencyError::Partition(_) => ("failed", "partition"),
                EmergencyError::Mismatch { .. } => ("failed", "mismatch"),
                EmergencyError::Edge(_) => ("failed", "store"),
            };
            json!({
                "emergency_read_format": 1,
                "outcome": outcome,
                "error": kind,
                "message": f.error.to_string(),
                "manifest": manifest(&f.manifest),
                "restart": restart,
                "restart_why": why,
            })
            .to_string()
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use super::*;
    use glassine_installer::emergency_read;
    use glassine_installer::fastboot::{self, FbError, Transport};
    use glassine_installer::glassine_read::Disk;
    use glassine_installer::inspect::{self, Host};
    use glassine_installer::sahara;
    use glassine_installer_usb as usb;
    use std::cell::RefCell;
    use std::rc::Rc;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::{JsFuture, future_to_promise, js_sys, spawn_local};

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

    /// Asks the browser for a device in its emergency download mode (USB
    /// 05c6:9008; call it from a click), reads what its boot ROM reports in
    /// Sahara's command mode and resolves with the record of it as JSON
    /// (`InfoReport`, never the serial). Sends queries only, no program; the
    /// device stays in the mode until its power key is held ~10 s. Rejects
    /// with an error record.
    #[wasm_bindgen]
    pub async fn emergency_info() -> Result<String, JsValue> {
        let (t, ids) = usb::request_kind(usb::Kind::Emergency)
            .await
            .map_err(|e| JsValue::from_str(&error(e.kind(), &e)))?;
        let read = async {
            let mut s = sahara::Sahara::connect(t).await?;
            let f = sahara::emergency_info(&mut s).await;
            s.into_transport().close().await;
            f
        };
        let f = read.await.map_err(|e| JsValue::from_str(&error("device", e)))?;
        Ok(serde_json::to_string(&sahara::InfoReport::new(&f, ids.vendor_id, ids.product_id, false)).unwrap())
    }

    /// Where the page stores an emergency read's images (web/lib/store.js
    /// over a folder the person picked): the steps of
    /// [`emergency_read::AsyncImageEdge`] by image name, each a promise but
    /// `write` (queued; it throws a failure of an earlier write), `progress`
    /// and `note`. `ready()` resolves once the queued bytes are few enough
    /// to read on: the read waits for it before each transfer from the
    /// device, so a slow disk slows the read instead of filling memory.
    #[wasm_bindgen]
    extern "C" {
        pub type ImageStore;
        #[wasm_bindgen(method, catch)]
        fn begin(this: &ImageStore, name: &str) -> Result<js_sys::Promise, JsValue>;
        #[wasm_bindgen(method, catch)]
        fn write(this: &ImageStore, data: &[u8]) -> Result<(), JsValue>;
        #[wasm_bindgen(method, catch)]
        fn end(this: &ImageStore, name: &str) -> Result<js_sys::Promise, JsValue>;
        #[wasm_bindgen(method, catch)]
        fn reopen(this: &ImageStore, name: &str) -> Result<js_sys::Promise, JsValue>;
        #[wasm_bindgen(method, catch)]
        fn read(this: &ImageStore, max: u32) -> Result<js_sys::Promise, JsValue>;
        #[wasm_bindgen(method, catch)]
        fn keep(this: &ImageStore, name: &str) -> Result<js_sys::Promise, JsValue>;
        #[wasm_bindgen(method, catch)]
        fn incomplete(this: &ImageStore, name: &str) -> Result<js_sys::Promise, JsValue>;
        #[wasm_bindgen(method, catch)]
        fn manifest(this: &ImageStore, text: &str) -> Result<js_sys::Promise, JsValue>;
        #[wasm_bindgen(method)]
        fn progress(this: &ImageStore, name: &str, done: f64, size: f64);
        #[wasm_bindgen(method)]
        fn note(this: &ImageStore, line: &str);
        #[wasm_bindgen(method)]
        fn ready(this: &ImageStore) -> js_sys::Promise;
    }

    /// A JS failure in words.
    fn words(e: JsValue) -> String {
        if let Some(t) = e.as_string() {
            return t;
        }
        js_sys::Reflect::get(&e, &JsValue::from_str("message"))
            .ok()
            .and_then(|m| m.as_string())
            .unwrap_or_else(|| format!("{e:?}"))
    }

    async fn settle(p: Result<js_sys::Promise, JsValue>) -> Result<JsValue, String> {
        JsFuture::from(p.map_err(words)?).await.map_err(words)
    }

    struct WebEdge(Rc<ImageStore>);

    impl emergency_read::AsyncImageEdge for WebEdge {
        async fn begin(&mut self, d: &Disk) -> Result<(), String> {
            settle(self.0.begin(&d.name)).await.map(|_| ())
        }
        fn write(&mut self, data: &[u8]) -> Result<(), String> {
            self.0.write(data).map_err(words)
        }
        async fn end(&mut self, d: &Disk) -> Result<(), String> {
            settle(self.0.end(&d.name)).await.map(|_| ())
        }
        async fn reopen(&mut self, d: &Disk) -> Result<(), String> {
            settle(self.0.reopen(&d.name)).await.map(|_| ())
        }
        async fn fill(&mut self, buf: &mut [u8]) -> Result<usize, String> {
            let v = settle(self.0.read(buf.len().min(u32::MAX as usize) as u32)).await?;
            let a = js_sys::Uint8Array::new(&v);
            let n = a.length() as usize;
            if n > buf.len() {
                return Err(format!("the store gave {n} bytes for {}", buf.len()));
            }
            a.copy_to(&mut buf[..n]);
            Ok(n)
        }
        async fn keep(&mut self, d: &Disk) -> Result<String, String> {
            let v = settle(self.0.keep(&d.name)).await?;
            v.as_string().ok_or_else(|| "the store named no file".into())
        }
        async fn incomplete(&mut self, d: &Disk) -> Result<String, String> {
            let v = settle(self.0.incomplete(&d.name)).await?;
            v.as_string().ok_or_else(|| "the store named no file".into())
        }
        async fn manifest(&mut self, text: &str) -> Result<(), String> {
            settle(self.0.manifest(text)).await.map(|_| ())
        }
        fn progress(&mut self, d: &Disk, done: u64) {
            self.0.progress(&d.name, done as f64, d.size as f64);
        }
        fn note(&mut self, line: &str) {
            self.0.note(line);
        }
    }

    /// The device's transport, waiting for the store before each transfer
    /// from the device; given back through `back` when the route drops it,
    /// so that its interface is let go of and waited for.
    struct Paced {
        t: Option<usb::UsbTransport>,
        store: Rc<ImageStore>,
        back: Rc<RefCell<Option<usb::UsbTransport>>>,
    }

    impl Paced {
        fn t(&mut self) -> Result<&mut usb::UsbTransport, FbError> {
            self.t.as_mut().ok_or_else(|| FbError::Usb("the connection is gone".into()))
        }
    }

    impl Drop for Paced {
        fn drop(&mut self) {
            *self.back.borrow_mut() = self.t.take();
        }
    }

    impl Transport for Paced {
        async fn send(&mut self, data: &[u8]) -> Result<(), FbError> {
            self.t()?.send(data).await
        }
        async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
            let _ = JsFuture::from(self.store.ready()).await;
            self.t()?.recv().await
        }
        async fn recv_max(&mut self, max: usize) -> Result<Vec<u8>, FbError> {
            let _ = JsFuture::from(self.store.ready()).await;
            self.t()?.recv_max(max).await
        }
        fn set_short_wait(&mut self, wait: Option<std::time::Duration>) {
            if let Some(t) = self.t.as_mut() {
                t.set_short_wait(wait);
            }
        }
    }

    /// The bound on one transfer during an emergency read: the command
    /// line's (the wait for the programmer's first answer, and each piece of
    /// a read).
    const EMERGENCY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

    /// Reads a device in its emergency download mode through the programmer
    /// the person supplied (spec 004) and stores the images in `store`:
    /// `device` and `file` name the description and its supplied file,
    /// `programmer` is the file's bytes (kept in the page; checked again here
    /// before anything opens a device, as on the command line, with
    /// `accept_unknown` the person's confirmation of the warning), `scope`
    /// one of [`super::emergency_scope`]'s. Then asks the browser for the
    /// device (USB 05c6:9008; call it from a click) and runs the core's
    /// route ([`emergency_read::run`]): the boot ROM's facts, the upload,
    /// Firehose, the reads (each range twice), the stored copy read back,
    /// the restart. Only list, read and restart reach the device (FR-007).
    /// Resolves with the run's record ([`super::emergency_record`]), a
    /// failed run included; rejects with an error record when nothing was
    /// started (the file, the scope, the chooser, the device's interface).
    #[wasm_bindgen]
    pub async fn emergency_read(
        device: String,
        file: String,
        programmer: Vec<u8>,
        accept_unknown: bool,
        scope: String,
        store: ImageStore,
    ) -> Result<String, JsValue> {
        let scope = super::emergency_scope(&scope).map_err(|e| JsValue::from_str(&e))?;
        super::emergency_programmer(&device, &file, &programmer, accept_unknown).map_err(|e| JsValue::from_str(&e))?;
        let (mut t, _ids) = usb::request_kind(usb::Kind::Emergency)
            .await
            .map_err(|e| JsValue::from_str(&error(e.kind(), &e)))?;
        t.set_timeout(EMERGENCY_TIMEOUT);
        let store = Rc::new(store);
        let back = Rc::new(RefCell::new(None));
        let paced = Paced {
            t: Some(t),
            store: store.clone(),
            back: back.clone(),
        };
        let mut edge = WebEdge(store);
        let r = emergency_read::run(paced, &programmer, &scope, emergency_read::Options::default(), &mut edge).await;
        let left = back.borrow_mut().take();
        if let Some(t) = left {
            t.close().await;
        }
        Ok(super::emergency_record(&r))
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
                let sent = s.reboot().await;
                s.into_transport().close().await;
                sent.map_err(|e| JsValue::from_str(&error("device", e)))?;
                Ok(JsValue::TRUE)
            })
        }

        /// Ends the session without sending anything (the device was
        /// unplugged, or another one is chosen) and lets go of the interface.
        pub fn close(&self) {
            if let Some(s) = self.session.borrow_mut().take() {
                spawn_local(s.into_transport().close());
            }
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

    /// The check record of a file the person picked (a `Uint8Array`), made
    /// here in the page; see `supplied_record`.
    #[wasm_bindgen]
    pub fn supplied_check(device: &str, file: &str, bytes: &[u8], reported: &str) -> Result<String, JsValue> {
        js(supplied_record(device, file, bytes, reported))
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
    fn a_supplied_file_check_is_a_record() {
        let r: Value = serde_json::from_str(&supplied_record("onyx-noteair", "emergency-programmer", b"not a programmer", "").unwrap()).unwrap();
        assert_eq!(r["supplied_check_format"], 1);
        assert_eq!(r["verdict"], "not-a-programmer");
        let dev = r#"{"chip": "0014d0e1", "pk_hash": "00"}"#;
        let r: Value = serde_json::from_str(&supplied_record("onyx-noteair", "emergency-programmer", b"x", dev).unwrap()).unwrap();
        assert_eq!(r["verdict"], "wrong-chip-or-key");
        for (d, f, rep) in [
            ("nope", "emergency-programmer", ""),
            ("onyx-noteair", "nope", ""),
            ("onyx-noteair", "emergency-programmer", "["),
        ] {
            let e: Value = serde_json::from_str(&supplied_record(d, f, b"", rep).unwrap_err()).unwrap();
            assert_eq!(e["error"], "input");
        }
    }

    /// The supplied fixtures of the core's tests (web/fixtures/supplied/).
    fn supplied(name: &str) -> Vec<u8> {
        let p = format!("{}/../web/fixtures/supplied/{name}.hex", env!("CARGO_MANIFEST_DIR"));
        let h: String = std::fs::read_to_string(p).unwrap().split_whitespace().collect();
        (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect()
    }

    /// 004 FR-004 again in the export: only a file that may be sent goes on,
    /// whatever the page said; an unknown one only with the confirmation.
    #[test]
    fn the_emergency_read_checks_the_file_again() {
        let fits = supplied("fits-unknown");
        assert_eq!(emergency_programmer("onyx-noteair", "emergency-programmer", &fits, true), Ok(()));
        let kind = |r: Result<(), String>| serde_json::from_str::<Value>(&r.unwrap_err()).unwrap()["error"].clone();
        assert_eq!(kind(emergency_programmer("onyx-noteair", "emergency-programmer", &fits, false)), "not-sendable");
        let junk = supplied("not-a-programmer");
        assert_eq!(kind(emergency_programmer("onyx-noteair", "emergency-programmer", &junk, true)), "not-sendable");
        assert_eq!(kind(emergency_programmer("nope", "emergency-programmer", &fits, true)), "input");
        assert_eq!(kind(emergency_programmer("onyx-noteair", "nope", &fits, true)), "input");
    }

    #[test]
    fn emergency_scopes() {
        assert_eq!(emergency_scope("whole-disk"), Ok(Scope::WholeDisk));
        assert_eq!(emergency_scope("boot-areas"), Ok(Scope::BootAreas));
        assert_eq!(
            emergency_scope("partitions:devinfo,boot_a"),
            Ok(Scope::Partitions(vec!["devinfo".into(), "boot_a".into()]))
        );
        for bad in ["", "gpt-only", "partitions:", "partitions:a,,b", "whole"] {
            let e: Value = serde_json::from_str(&emergency_scope(bad).unwrap_err()).unwrap();
            assert_eq!(e["error"], "input", "{bad}");
        }
    }

    /// The outcome kinds the Back up screen acts on: only a file the device
    /// didn't take or that didn't answer is `refused` (004 FR-006).
    #[test]
    fn emergency_records_by_outcome() {
        use glassine_installer::firehose::FirehoseError;
        use glassine_installer::sahara::SaharaError;
        let fail = |error| Failure {
            error,
            restart: Restart::NotPossible,
            manifest: None,
        };
        for (e, outcome, kind) in [
            (EmergencyError::Refused("x".into()), "refused", "refused"),
            (EmergencyError::NotWorking("x".into()), "refused", "not-working"),
            (
                EmergencyError::BootRom(SaharaError::Transport(glassine_installer::fastboot::FbError::Timeout("x".into()))),
                "nothing-sent",
                "boot-rom",
            ),
            (EmergencyError::UnknownChip("x".into()), "nothing-sent", "unknown-chip"),
            (EmergencyError::Firehose(FirehoseError::NoAnswer), "failed", "programmer"),
            (EmergencyError::Storage("x".into()), "failed", "storage"),
            (EmergencyError::Partition("x".into()), "failed", "partition"),
            (
                EmergencyError::Mismatch {
                    image: "emmc-user".into(),
                    offset: 0,
                },
                "failed",
                "mismatch",
            ),
            (EmergencyError::Edge("disk full".into()), "failed", "store"),
        ] {
            let r: Value = serde_json::from_str(&emergency_record(&Err(fail(e)))).unwrap();
            assert_eq!(r["emergency_read_format"], 1);
            assert_eq!((r["outcome"].as_str().unwrap(), r["error"].as_str().unwrap()), (outcome, kind));
            assert_eq!(r["restart"], "by-hand");
            assert!(r["manifest"].is_null());
        }
        let r: Value = serde_json::from_str(&emergency_record(&Err(Failure {
            error: EmergencyError::Edge("disk full".into()),
            restart: Restart::Failed("no answer".into()),
            manifest: Some("{\"manifest_format\": 1}".into()),
        })))
        .unwrap();
        assert_eq!((r["restart"].as_str(), r["restart_why"].as_str()), (Some("failed"), Some("no answer")));
        assert_eq!(r["manifest"]["manifest_format"], 1);
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
