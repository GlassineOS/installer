// SPDX-License-Identifier: GPL-2.0-or-later
//! The installation in the browser: the core's executor ([`execute::run`])
//! with the page as its edge. The records the page gets are JSON:
//!
//! - `install_new(device, facts, answers, manifest, confirmation, at)`: the
//!   install record of a confirmed plan, as the command line's `install`
//!   makes it: the plan again from the facts, the person's answers and the
//!   release's manifest (so it is the plan the screens showed), the artifacts
//!   its steps use as the manifest states them, the description's hash. The
//!   page saves it before the run starts.
//! - `record_view(record)`: a saved record checked and summed up for the
//!   screens (`record_view_format` 1): where it stands, the plan it holds.
//! - `install_run(record, developer, release, host)`: runs or resumes the
//!   record's plan and resolves with how it ended (`install_run_format` 1):
//!   `waiting` (every step done, the device restarting into GlassineOS, which
//!   the person checks), `complete`, or `stopped` (the step, why, the step's
//!   escape from the description, whether a write may have reached the
//!   device). Every outcome carries the record as it was last saved.
//!
//! The executor is the one the command line runs. Only the edge is the
//! page's: the device's USB connections (each mode is its own USB device, and
//! a new one needs the person's permission in the browser's chooser), the
//! record and a boot image's original in the folder the person picked, the
//! backup read from its folder, the release's files from theirs.

use glassine_installer::artifacts::{Artifacts, MemorySource};
use glassine_installer::execute::{Outcome, Stopped};
use glassine_installer::format1::Description;
use glassine_installer::lock::sha256_hex;
use glassine_installer::plan::{self, Status};
use glassine_installer::recognize;
use glassine_installer::record::{Confirmation, Record, Resume, StepState};
use glassine_installer::release::Release;
use glassine_installer::validate::{self, Loaded};
use serde_json::{Value, json};
use std::collections::BTreeMap;

use super::{descriptions, error, input};

/// The format of [`run_record`]'s record.
pub const RUN_FORMAT: i64 = 1;
/// The format of [`record_view`]'s record.
pub const VIEW_FORMAT: i64 = 1;

/// Polls a future that is ready at once (a source held in memory).
fn ready<F: Future>(f: F) -> F::Output {
    let mut f = std::pin::pin!(f);
    match f.as_mut().poll(&mut std::task::Context::from_waker(std::task::Waker::noop())) {
        std::task::Poll::Ready(v) => v,
        std::task::Poll::Pending => unreachable!("a source in memory never waits"),
    }
}

/// The shipped description `id` and the hash of its source, the one a record
/// made in this page names.
pub fn description_of(id: &str) -> Result<(Description, String), String> {
    let src = super::description_source(id).ok_or_else(|| error("descriptions", format!("this installer has no description {id:?}")))?;
    match validate::load(src) {
        Loaded::Valid(d) => Ok((*d, sha256_hex(src.as_bytes()))),
        _ => Err(error("descriptions", format!("the description {id:?} doesn't load"))),
    }
}

/// The install record of a confirmed plan: see the module. `answers` is the
/// plan's input as [`super::input`] reads it (without `release`); `manifest`
/// the release's `release.json`. Refused unless the plan is ready (or
/// developer-only and asked for by a developer) and the release supplies every
/// artifact its steps use.
pub fn new_record(device: &str, facts: &str, answers: &str, manifest: &str, confirmation: &str, at: Option<String>) -> Result<String, String> {
    let (d, _) = description_of(device)?;
    let src = super::description_source(device).unwrap();
    let ds = descriptions();
    let refs: Vec<&Description> = ds.iter().collect();
    let f = glassine_installer::facts::Snapshot::parse(facts).map_err(|e| error("facts", e))?;
    let mut i = input(answers)?;
    i.release = Some(Release::parse(manifest).map_err(|e| error("input", format!("the release's manifest: {e}")))?);
    let rec = recognize::recognize(&refs, &f);
    let p = plan::plan(&refs, &f, &rec, &i);
    if p.description != device {
        return Err(error("input", format!("the plan is for {:?}, not {device}", p.description)));
    }
    match p.status {
        Status::Ready => {}
        Status::DeveloperOnly if i.developer => {}
        _ => {
            return Err(error(
                "input",
                format!("this plan is {}: it isn't run", serde_json::to_value(&p.status).unwrap()),
            ));
        }
    }
    let plan_json = serde_json::to_string(&p).unwrap();
    // The artifacts the plan's steps use, as the release states them.
    let mut ms = MemorySource {
        origin: "the release you chose".into(),
        files: BTreeMap::new(),
    };
    ms.files.insert(glassine_installer::artifacts::MANIFEST.into(), manifest.as_bytes().to_vec());
    let a = ready(Artifacts::open(ms)).map_err(|e| error("input", e))?;
    let mut roles: Vec<String> = vec![];
    for s in &p.steps {
        if let Some(r) = d
            .steps
            .iter()
            .find(|x| x.id == s.id)
            .and_then(|x| x.params.get("artifact"))
            .and_then(|v| v.as_str())
            && !roles.iter().any(|x| x == r)
        {
            roles.push(r.to_string());
        }
    }
    let arts = roles
        .iter()
        .map(|r| a.artifact(r).map_err(|e| error("input", e)))
        .collect::<Result<Vec<_>, _>>()?;
    let r = Record::new(
        env!("CARGO_PKG_VERSION"),
        &d.id,
        src.as_bytes(),
        &plan_json,
        Confirmation { text: confirmation.into(), at },
        arts,
        None,
    )
    .map_err(|e| error("input", e))?;
    Ok(r.to_json())
}

/// The steps a run went on past (refused before they wrote anything), with why.
fn went_on_past(r: &Record) -> Vec<Value> {
    r.plan_steps()
        .unwrap_or_default()
        .into_iter()
        .filter(|s| r.state(s) == StepState::Failed)
        .map(|s| {
            let why = r
                .journal
                .iter()
                .rev()
                .find(|e| e.step == s)
                .and_then(|e| e.outcome.as_ref())
                .and_then(|o| o.message.clone());
            json!({ "step": s, "why": why })
        })
        .collect()
}

fn state_word(s: StepState) -> &'static str {
    match s {
        StepState::NotStarted => "not-started",
        StepState::Open => "open",
        StepState::Done => "done",
        StepState::Failed => "failed",
        StepState::Interrupted => "interrupted",
    }
}

/// A saved record checked ([`Record::parse`]) and summed up for the screens:
/// each plan step's state, what a resume does next, the backup, the final
/// restart, and the plan itself (its JSON, as confirmed).
pub fn record_view(record: &str) -> Result<String, String> {
    let r = Record::parse(record).map_err(|e| error("input", e))?;
    let steps = r.plan_steps().map_err(|e| error("input", e))?;
    let (next, step) = match r.resume().map_err(|e| error("input", e))? {
        Resume::Interrupted(s) => ("interrupted", Some(s)),
        Resume::Failed(s) => ("failed", Some(s)),
        Resume::Next(s) => ("next", Some(s)),
        Resume::Complete => ("complete", None),
    };
    let last = |s: &str| {
        r.journal
            .iter()
            .rev()
            .find(|e| e.step == s)
            .and_then(|e| e.outcome.as_ref())
            .and_then(|o| o.message.clone())
    };
    let known = description_of(&r.description.id).map(|(_, sha)| sha == r.description.sha256).unwrap_or(false);
    Ok(json!({
        "record_view_format": VIEW_FORMAT,
        "description": r.description.id,
        "same_description": known,
        "installer_version": r.installer_version,
        "plan": serde_json::from_str::<Value>(&r.plan).map_err(|e| error("input", e))?,
        "steps": steps.iter().map(|s| json!({ "step": s, "state": state_word(r.state(s)), "message": last(s) })).collect::<Vec<_>>(),
        "resume": { "next": next, "step": step },
        "backup": r.backup,
        "artifacts": r.artifacts,
        "final_restart": r.final_restart,
        "went_on_past": went_on_past(&r),
    })
    .to_string())
}

/// How a run ended, as a record: see the module.
pub fn run_record(r: &Result<Outcome, Stopped>, rec: &Record) -> String {
    let record = rec.to_json();
    match r {
        Ok(Outcome::Complete) => json!({ "install_run_format": RUN_FORMAT, "outcome": "complete", "went_on_past": went_on_past(rec), "record": record }),
        Ok(Outcome::WaitingForAcceptance { mode, check }) => json!({
            "install_run_format": RUN_FORMAT, "outcome": "waiting", "mode": mode, "check": check,
            "went_on_past": went_on_past(rec), "record": record,
        }),
        Err(s) => json!({
            "install_run_format": RUN_FORMAT,
            "outcome": "stopped",
            "step": s.step,
            "why": s.why,
            "escape": s.escape.as_ref().map(|e| json!({ "leaves": e.leaves, "hardware": e.hardware, "way_out": e.way_out, "text": e.text })),
            "may_have_changed": s.may_have_changed,
            "went_on_past": went_on_past(rec),
            "record": record,
        }),
    }
    .to_string()
}

/// The job of a record's plan (its `job`).
pub fn job_id(rec: &Record) -> Result<String, String> {
    serde_json::from_str::<Value>(&rec.plan)
        .ok()
        .and_then(|v| v.get("job").and_then(|j| j.as_str()).map(String::from))
        .ok_or_else(|| error("input", "the record's plan names no job"))
}

#[cfg(target_arch = "wasm32")]
pub mod web {
    use super::*;
    use glassine_installer::artifacts::ArtifactSource;
    use glassine_installer::emergency_read::USER_AREA_NAME;
    use glassine_installer::execute::{self, InstallEdge, Job, Link, Reach};
    use glassine_installer::fastboot::{FbError, Transport};
    use glassine_installer::format1::{Mode, Step};
    use glassine_installer::record::{self, AsyncRecordStore, Backup};
    use glassine_installer_usb as usb;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;
    use wasm_bindgen::prelude::*;
    use wasm_bindgen_futures::{JsFuture, js_sys};

    /// The RAM environment bounds one operation at 120 s (as the command line).
    const RAM_TRANSFER_TIMEOUT: Duration = Duration::from_secs(130);

    /// The page's side of a run (web/lib/install.js): each a promise but
    /// `progress`, `note`, `cancel_requested` and `now`.
    /// - `reach(mode, how)`: `mode` is the description's mode (JSON), `how`
    ///   `{"kind": "already"|"sent"|"perform"|"ask", "transition", "text"}`.
    ///   Resolves with `{link: "fastboot"|"ram", device: USBDevice}` (a
    ///   device the person allowed) or `{link: "elsewhere", mode: ID}`;
    ///   rejects with why (the person stopped, nothing came).
    /// - `write_temp(bytes)`, `commit()`, `read()`: the record's file in the
    ///   folder (a whole copy under a temporary name, then renamed over it).
    /// - `back_up(step)`: `{location, manifest: bytes|null, user_area: bool}`
    ///   of the backup folder.
    /// - `backup_bytes(offset, len)`: bytes of the backed-up user area.
    /// - `keep(name, bytes)`, `kept(name)`: a step's original, beside the record.
    #[wasm_bindgen]
    extern "C" {
        pub type InstallHost;
        #[wasm_bindgen(method, catch)]
        fn reach(this: &InstallHost, mode: &str, how: &str) -> Result<js_sys::Promise, JsValue>;
        #[wasm_bindgen(method, catch)]
        fn write_temp(this: &InstallHost, bytes: &[u8]) -> Result<js_sys::Promise, JsValue>;
        #[wasm_bindgen(method, catch)]
        fn commit(this: &InstallHost) -> Result<js_sys::Promise, JsValue>;
        #[wasm_bindgen(method, catch)]
        fn read(this: &InstallHost) -> Result<js_sys::Promise, JsValue>;
        #[wasm_bindgen(method, catch)]
        fn back_up(this: &InstallHost, step: &str) -> Result<js_sys::Promise, JsValue>;
        #[wasm_bindgen(method, catch)]
        fn backup_bytes(this: &InstallHost, offset: f64, len: u32) -> Result<js_sys::Promise, JsValue>;
        #[wasm_bindgen(method, catch)]
        fn keep(this: &InstallHost, name: &str, bytes: &[u8]) -> Result<js_sys::Promise, JsValue>;
        #[wasm_bindgen(method, catch)]
        fn kept(this: &InstallHost, name: &str) -> Result<js_sys::Promise, JsValue>;
        #[wasm_bindgen(method)]
        fn progress(this: &InstallHost, step: &str, done: f64, total: f64);
        #[wasm_bindgen(method)]
        fn note(this: &InstallHost, line: &str);
        #[wasm_bindgen(method)]
        fn cancel_requested(this: &InstallHost) -> bool;
        #[wasm_bindgen(method)]
        fn now(this: &InstallHost) -> Option<String>;

        /// The release's folder (web/lib/install.js): `size(file)` and
        /// `read(file, offset, len)`, promises; `origin()` its name.
        pub type ReleaseFiles;
        #[wasm_bindgen(method)]
        fn origin(this: &ReleaseFiles) -> String;
        #[wasm_bindgen(method, catch, js_name = size)]
        fn file_size(this: &ReleaseFiles, file: &str) -> Result<js_sys::Promise, JsValue>;
        #[wasm_bindgen(method, catch, js_name = read)]
        fn read_file(this: &ReleaseFiles, file: &str, offset: f64, len: u32) -> Result<js_sys::Promise, JsValue>;
    }

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

    fn bytes(v: &JsValue) -> Option<Vec<u8>> {
        if v.is_null() || v.is_undefined() {
            None
        } else {
            Some(js_sys::Uint8Array::new(v).to_vec())
        }
    }

    fn get(o: &JsValue, k: &str) -> JsValue {
        js_sys::Reflect::get(o, &JsValue::from_str(k)).unwrap_or(JsValue::UNDEFINED)
    }

    struct Release(ReleaseFiles);

    impl ArtifactSource for Release {
        fn origin(&self) -> String {
            self.0.origin()
        }
        async fn size(&mut self, file: &str) -> Result<u64, String> {
            let v = settle(self.0.file_size(file)).await?;
            v.as_f64().map(|n| n as u64).ok_or_else(|| format!("{file}: no size"))
        }
        async fn read_at(&mut self, file: &str, offset: u64, buf: &mut [u8]) -> Result<(), String> {
            let v = settle(self.0.read_file(file, offset as f64, buf.len() as u32)).await?;
            let a = js_sys::Uint8Array::new(&v);
            if a.length() as usize != buf.len() {
                return Err(format!("{file}: {} bytes read at {offset}, not {}", a.length(), buf.len()));
            }
            a.copy_to(buf);
            Ok(())
        }
    }

    /// A device's transport, given back to the edge when the executor lets go
    /// of it (a mode left, a run ended), so that its interface is released and
    /// waited for: dropped, the browser's rejection would go unhandled.
    struct Held {
        t: Option<usb::UsbTransport>,
        left: Rc<RefCell<Vec<usb::UsbTransport>>>,
    }

    impl Held {
        fn t(&mut self) -> Result<&mut usb::UsbTransport, FbError> {
            self.t.as_mut().ok_or_else(|| FbError::Usb("the connection is gone".into()))
        }
    }

    impl Drop for Held {
        fn drop(&mut self) {
            if let Some(t) = self.t.take() {
                self.left.borrow_mut().push(t);
            }
        }
    }

    impl Transport for Held {
        async fn send(&mut self, data: &[u8]) -> Result<(), FbError> {
            self.t()?.send(data).await
        }
        async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
            self.t()?.recv().await
        }
        async fn recv_max(&mut self, max: usize) -> Result<Vec<u8>, FbError> {
            self.t()?.recv_max(max).await
        }
    }

    struct Store(Rc<InstallHost>);

    impl AsyncRecordStore for Store {
        async fn write_temp(&mut self, b: &[u8]) -> Result<(), String> {
            settle(self.0.write_temp(b)).await.map(|_| ())
        }
        async fn commit(&mut self) -> Result<(), String> {
            settle(self.0.commit()).await.map(|_| ())
        }
        async fn read(&mut self) -> Result<Option<Vec<u8>>, String> {
            Ok(bytes(&settle(self.0.read()).await?))
        }
    }

    struct Edge {
        host: Rc<InstallHost>,
        left: Rc<RefCell<Vec<usb::UsbTransport>>>,
    }

    impl Edge {
        /// Lets go of the interfaces the executor dropped.
        async fn close_left(&mut self) {
            let left: Vec<_> = self.left.borrow_mut().drain(..).collect();
            for t in left {
                t.close().await;
            }
        }

        fn held(&self, t: usb::UsbTransport) -> Held {
            Held {
                t: Some(t),
                left: self.left.clone(),
            }
        }
    }

    fn how_json(how: &Reach) -> String {
        match how {
            Reach::Already { text } => json!({ "kind": "already", "text": text }),
            Reach::Sent { transition } => json!({ "kind": "sent", "transition": transition }),
            Reach::Perform { transition, text } => json!({ "kind": "perform", "transition": transition, "text": text }),
            Reach::AskPerson { transition, text } => json!({ "kind": "ask", "transition": transition, "text": text }),
        }
        .to_string()
    }

    impl InstallEdge for Edge {
        type Fb = Held;
        type Ram = Held;

        async fn reach(&mut self, mode: &Mode, how: Reach) -> Result<Link<Held, Held>, String> {
            self.close_left().await;
            let m = serde_json::to_string(mode).unwrap();
            let got = settle(self.host.reach(&m, &how_json(&how))).await?;
            let link = get(&got, "link").as_string().unwrap_or_default();
            let kind = match link.as_str() {
                "fastboot" => usb::Kind::Fastboot,
                "ram" => usb::Kind::RamRead,
                "elsewhere" => return Ok(Link::Elsewhere(get(&got, "mode").as_string().ok_or("the page named no mode")?)),
                l => return Err(format!("the page answered {l:?}")),
            };
            let device = get(&got, "device");
            if device.is_undefined() || device.is_null() {
                return Err("the page gave no device".into());
            }
            let (mut t, _) = usb::open_device(device.unchecked_into(), kind).await.map_err(|e| e.to_string())?;
            Ok(match kind {
                usb::Kind::RamRead => {
                    t.set_timeout(RAM_TRANSFER_TIMEOUT);
                    Link::Ram(self.held(t))
                }
                _ => Link::Fastboot(self.held(t)),
            })
        }

        async fn save(&mut self, r: &Record) -> Result<(), String> {
            record::save_async(&mut Store(self.host.clone()), r).await
        }

        async fn back_up(&mut self, step: &Step) -> Result<Backup, String> {
            let v = settle(self.host.back_up(&step.id)).await?;
            let location = get(&v, "location").as_string().ok_or("the backup has no folder")?;
            let manifest = bytes(&get(&v, "manifest")).ok_or_else(|| format!("{location} has no manifest.json: a finished backup has its manifest"))?;
            if !get(&v, "user_area").is_truthy() {
                return Err(format!(
                    "{location} has no {USER_AREA_NAME}.img.gz: the backup doesn't hold the whole user area"
                ));
            }
            Ok(Backup {
                location,
                manifest_sha256: Some(sha256_hex(&manifest)),
            })
        }

        async fn backup_bytes(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, String> {
            let v = settle(self.host.backup_bytes(offset as f64, len as u32)).await?;
            let b = bytes(&v).ok_or("the backup gave nothing")?;
            if b.len() != len {
                return Err(format!("the backup gave {} bytes at {offset}, not {len}", b.len()));
            }
            Ok(b)
        }

        async fn keep(&mut self, name: &str, b: &[u8]) -> Result<(), String> {
            if !glassine_installer::artifacts::plain_path(name) || name.contains('/') {
                return Err(format!("{name:?} isn't a plain file name"));
            }
            settle(self.host.keep(name, b)).await.map(|_| ())
        }

        async fn kept(&mut self, name: &str) -> Result<Option<Vec<u8>>, String> {
            Ok(bytes(&settle(self.host.kept(name)).await?))
        }

        fn progress(&mut self, step: &str, done: u64, total: u64) {
            self.host.progress(step, done as f64, total as f64);
        }

        fn note(&mut self, line: &str) {
            self.host.note(line);
        }

        fn cancel_requested(&mut self) -> bool {
            self.host.cancel_requested()
        }

        fn now(&mut self) -> Option<String> {
            self.host.now()
        }
    }

    /// The install record of a confirmed plan (see [`super::new_record`]).
    #[wasm_bindgen]
    pub fn install_new(device: &str, facts: &str, answers: &str, manifest: &str, confirmation: &str, at: Option<String>) -> Result<String, JsValue> {
        super::new_record(device, facts, answers, manifest, confirmation, at).map_err(|e| JsValue::from_str(&e))
    }

    /// A saved record checked and summed up (see [`super::record_view`]).
    #[wasm_bindgen]
    pub fn install_record_view(record: &str) -> Result<String, JsValue> {
        super::record_view(record).map_err(|e| JsValue::from_str(&e))
    }

    /// Runs or resumes the plan of `record` (the record as saved) with the
    /// release in `release` and the page as the edge; resolves with the run's
    /// record ([`super::run_record`]); rejects with an error record when
    /// nothing was started (the record, the description, the release's
    /// manifest).
    #[wasm_bindgen]
    pub async fn install_run(record: String, developer: bool, release: ReleaseFiles, host: InstallHost) -> Result<String, JsValue> {
        let fail = |e: String| JsValue::from_str(&e);
        let mut rec = Record::parse(&record).map_err(|e| fail(error("input", e)))?;
        let (d, sha) = description_of(&rec.description.id).map_err(fail)?;
        let job = Job::new(&d, sha, developer, &job_id(&rec).map_err(fail)?).map_err(|e| fail(error("descriptions", e)))?;
        let mut artifacts = Artifacts::open(Release(release)).await.map_err(|e| fail(error("input", e)))?;
        let mut edge = Edge {
            host: Rc::new(host),
            left: Rc::new(RefCell::new(vec![])),
        };
        let plan = rec.plan.clone();
        let r = execute::run(&job, &plan, &mut rec, &mut artifacts, &mut edge).await;
        edge.close_left().await;
        Ok(super::run_record(&r, &rec))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{}/../core/tests/fixtures/facts/{name}.json", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    /// A release manifest with every role the Note Air's install job uses.
    fn manifest() -> String {
        let mut m = serde_json::Map::new();
        for (role, kind) in [
            ("boot-loader", "raw-image"),
            ("ram-environment", "raw-image"),
            ("partition-layout", "gpt-recipe"),
            ("kernel", "raw-image"),
            ("base", "btrfs-pool-image"),
            ("recovery", "raw-image"),
        ] {
            m.insert(
                role.into(),
                json!({"file": format!("{role}.img"), "sha256": "ab".repeat(32), "size": 1024, "kind": kind}),
            );
        }
        json!({"release_format": 1, "artifacts": m}).to_string()
    }

    const DEV: &str = r#"{"choices": {"keep-stock": "no"}, "hardware": {"emergency-cable": true}, "developer": true}"#;

    #[test]
    fn a_new_record_holds_the_plan_the_screens_showed_and_the_releases_artifacts() {
        let f = fixture("noteair-stock-fastboot");
        let r = Record::parse(&new_record("onyx-noteair", &f, DEV, &manifest(), "Install as planned", None).unwrap()).unwrap();
        // The plan the screens get for the same facts, answers and release.
        let mut answers: Value = serde_json::from_str(DEV).unwrap();
        answers["release"] = serde_json::from_str(&manifest()).unwrap();
        assert_eq!(r.plan, super::super::plan_record(&f, &answers.to_string()).unwrap());
        assert_eq!(r.description.id, "onyx-noteair");
        assert_eq!(r.description.sha256, description_of("onyx-noteair").unwrap().1);
        let roles: Vec<&str> = r.artifacts.iter().map(|a| a.role.as_str()).collect();
        for want in ["boot-loader", "ram-environment", "partition-layout", "kernel", "base"] {
            assert!(roles.contains(&want), "{want} in {roles:?}");
        }
        assert!(r.journal.is_empty() && r.backup.is_none());
        assert_eq!(job_id(&r).unwrap(), "install");
        let v: Value = serde_json::from_str(&record_view(&r.to_json()).unwrap()).unwrap();
        assert_eq!(v["record_view_format"], 1);
        assert_eq!(v["resume"]["next"], "next");
        assert_eq!(v["resume"]["step"], "back-up");
        assert_eq!(v["same_description"], true);
        assert!(v["steps"].as_array().unwrap().iter().all(|s| s["state"] == "not-started"));
    }

    #[test]
    fn a_plan_that_isnt_run_makes_no_record() {
        let f = fixture("noteair-stock-fastboot");
        // For people the Note Air is refused while its recovery isn't built.
        let e: Value = serde_json::from_str(&new_record("onyx-noteair", &f, "", &manifest(), "x", None).unwrap_err()).unwrap();
        assert_eq!(e["error"], "input");
        // A release without the kernel.
        let mut m: Value = serde_json::from_str(&manifest()).unwrap();
        m["artifacts"].as_object_mut().unwrap().remove("kernel");
        let e: Value = serde_json::from_str(&new_record("onyx-noteair", &f, DEV, &m.to_string(), "x", None).unwrap_err()).unwrap();
        assert!(e["message"].as_str().unwrap().contains("kernel"), "{e}");
        // Another device.
        assert!(new_record("uefi-pc", &f, DEV, &manifest(), "x", None).is_err());
        assert!(record_view("{}").is_err());
    }

    #[test]
    fn a_run_ends_as_a_record() {
        let f = fixture("noteair-stock-fastboot");
        let mut r = Record::parse(&new_record("onyx-noteair", &f, DEV, &manifest(), "x", None).unwrap()).unwrap();
        let v: Value = serde_json::from_str(&run_record(
            &Ok(Outcome::WaitingForAcceptance {
                mode: "glassine".into(),
                check: "Start the device normally.".into(),
            }),
            &r,
        ))
        .unwrap();
        assert_eq!((v["install_run_format"].as_i64(), v["outcome"].as_str()), (Some(1), Some("waiting")));
        assert_eq!(Record::parse(v["record"].as_str().unwrap()).unwrap(), r);
        r.begin("back-up", "", None).unwrap();
        r.finish(
            "back-up",
            glassine_installer::record::Outcome {
                result: glassine_installer::record::Result_::Failed,
                evidence: Default::default(),
                message: Some("no room".into()),
                at: None,
            },
        )
        .unwrap();
        let s = Stopped {
            step: Some("back-up".into()),
            why: "no room".into(),
            escape: Some(Box::new(glassine_installer::execute::Escape {
                leaves: "previous-system-boots".into(),
                hardware: None,
                way_out: "transition emergency-to-android".into(),
                text: Some("it restarts".into()),
            })),
            may_have_changed: false,
        };
        let v: Value = serde_json::from_str(&run_record(&Err(s), &r)).unwrap();
        assert_eq!(v["outcome"], "stopped");
        assert_eq!(v["step"], "back-up");
        assert_eq!(v["escape"]["leaves"], "previous-system-boots");
        assert_eq!(v["went_on_past"][0]["why"], "no room");
        let rv: Value = serde_json::from_str(&record_view(v["record"].as_str().unwrap()).unwrap()).unwrap();
        assert_eq!(
            (rv["resume"]["next"].as_str(), rv["resume"]["step"].as_str()),
            (Some("failed"), Some("back-up"))
        );
        assert_eq!(rv["steps"][0]["message"], "no room");
    }
}
