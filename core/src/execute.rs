// SPDX-License-Identifier: GPL-2.0-or-later
//! The executor: runs a confirmed plan's steps on the device, one at a time,
//! with the install record as its journal (spec 001 FR-006, FR-007, SC-003).
//!
//! For each step, in the plan's order and after the steps it names in
//! `after`:
//! 1. its `done` conditions are evaluated from what is already known (the
//!    record, the mode the device is in): a step that is done is recorded as
//!    found done and skipped;
//! 2. the device is moved into the step's mode, only along the
//!    description's transitions (the installer's own commands where it can,
//!    the edge for the rest: `adb`, the person's hands); `done` is evaluated
//!    again there;
//! 3. the intent (what the step is about to do, the artifact's hash) is
//!    recorded and saved BEFORE anything is written;
//! 4. the step's op runs ([`crate::ops`]);
//! 5. `done` is evaluated after it (moving to the mode its conditions are
//!    read in, if needed): only a step whose `done` holds is recorded done,
//!    with the op's evidence.
//!
//! Anything that doesn't go as the description says stops the run with the
//! step, why, and the step's escape from the description ([`Stopped`]).
//! Nothing is substituted or retried behind the person's back. A cancel is
//! honoured only after a step that is a safe stop.
//!
//! Resuming: the record's plan must be the confirmed one (its hash), the
//! description the recorded one, every artifact the recorded one. A step
//! that was cut while it ran (an intent without an outcome) runs again
//! whole: every write is a whole-partition or whole-table rewrite of the
//! same bytes. The one exception is the boot-loader step, whose old mode is
//! gone once it succeeded: its `done` condition (the boot loader that
//! answers) decides whether it is redone.
//!
//! The RAM environment arms its writable ranges once per start: on entering
//! it the executor arms every range the plan's remaining steps in that mode
//! write (from the new table when the table step is still to come), each the
//! extent of a partition the step writes and none never-write. Found armed
//! (a run cut while the environment kept running), it is restarted through
//! the boot loader first.
//!
//! The device and the computer are the edge's ([`InstallEdge`]): the USB
//! waits, `adb`, the person's answers, where the record is saved.

use crate::artifacts::{ArtifactSource, Artifacts};
use crate::facts::{InstallerFacts, Obs, Snapshot, Storage, Truth, VarFacts, test};
use crate::fastboot::{FbError, Reply, Session, Transport};
use crate::format1::{Condition, Description, EscapeCase, Mode, Step};
use crate::glassine_read::Reboot;
use crate::glassine_write::{RamError, Range, STATUS_ALREADY_ARMED, Writer, Writes};
use crate::gpt_build::Built;
use crate::inspect::{choose_framing, framing_table};
use crate::lock::sha256_hex;
use crate::ops::{self, Evidence, Gpt, OpError};
use crate::record::{Backup, FinalRestart, Outcome as Done, Record, Result_, StepState};
use crate::vocab::INSTALLER_MODE;
use crate::write_rules::ModeRules;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

/// How the edge should get the device into a mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// Where the device is isn't known (the start of a run, a resume): find
    /// it in this mode or in any mode the description has, and tell the
    /// person `text` (the mode's text) while nothing turns up.
    Already { text: Option<String> },
    /// The installer has sent the command that restarts the device into the
    /// mode (`transition`, or `None` for the mode restarting itself): wait for it.
    Sent { transition: Option<String> },
    /// The edge performs the transition (the installer can't from the core:
    /// `adb reboot bootloader`); if it can't, it tells the person `text`.
    Perform { transition: String, text: Option<String> },
    /// The person does it by hand, as `text` says.
    AskPerson { transition: Option<String>, text: String },
}

/// What the edge found.
pub enum Link<F, R> {
    /// A fastboot interface (any fastboot mode: the boot loader that answers
    /// tells which).
    Fastboot(F),
    /// The RAM environment's interface.
    Ram(R),
    /// The device is in this mode, which the installer can't talk to (the
    /// edge can tell, e.g. through adb).
    Elsewhere(String),
}

/// The computer's side of an installation.
pub trait InstallEdge {
    type Fb: Transport;
    type Ram: Transport;
    /// Waits for the device in `mode` (or, with [`Reach::Already`], in any
    /// mode), after doing what `how` says.
    fn reach(&mut self, mode: &Mode, how: Reach) -> impl Future<Output = Result<Link<Self::Fb, Self::Ram>, String>>;
    /// Saves the record atomically ([`crate::record::save`] through a store),
    /// done before it resolves: the executor sends nothing to the device
    /// while a save is pending.
    fn save(&mut self, record: &Record) -> impl Future<Output = Result<(), String>>;
    /// Runs or finds the step's backup and checks it; the backup's location
    /// and its manifest's hash once verified.
    fn back_up(&mut self, step: &Step) -> impl Future<Output = Result<Backup, String>>;
    /// Bytes of the backed-up disk the partition table is on (the verified
    /// backup's image), from `offset`.
    fn backup_bytes(&mut self, offset: u64, len: usize) -> impl Future<Output = Result<Vec<u8>, String>>;
    /// Keeps `bytes` on the computer under `name`, whole before it returns:
    /// what a step read before it first wrote (a boot image's original).
    fn keep(&mut self, name: &str, bytes: &[u8]) -> impl Future<Output = Result<(), String>>;
    /// What [`InstallEdge::keep`] kept under `name`, if anything.
    fn kept(&mut self, name: &str) -> impl Future<Output = Result<Option<Vec<u8>>, String>>;
    fn progress(&mut self, step: &str, done: u64, total: u64);
    fn note(&mut self, line: &str);
    /// True once the person asked to stop; honoured after a safe-stop step.
    fn cancel_requested(&mut self) -> bool;
    /// The time, as text, for the record (the core has no clock).
    fn now(&mut self) -> Option<String> {
        None
    }
}

/// What the run needs beyond the record.
pub struct Job<'a> {
    pub description: &'a Description,
    /// The SHA-256 of the description file as read: must be the record's.
    pub description_sha256: String,
    /// The person is a developer who asked for untried write steps.
    pub developer: bool,
    /// The write rules of each fastboot mode, from the description
    /// ([`crate::write_rules::mode_rules`]); a write in a mode without them
    /// is refused.
    pub write_rules: BTreeMap<String, ModeRules>,
}

impl<'a> Job<'a> {
    /// A job for `description` with the write rules of every fastboot mode
    /// it has, for the steps of job `job` (the plan's).
    pub fn new(description: &'a Description, description_sha256: String, developer: bool, job: &str) -> Result<Job<'a>, String> {
        let mut write_rules = BTreeMap::new();
        for m in description.modes.iter().filter(|m| m.kind == "fastboot") {
            write_rules.insert(m.id.clone(), crate::write_rules::mode_rules(description, &m.id, job)?);
        }
        Ok(Job {
            description,
            description_sha256,
            developer,
            write_rules,
        })
    }
}

/// How a run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Every step done and the device in a mode that was identified.
    Complete,
    /// Every step done and the device restarted into a system the installer
    /// can't identify yet: the person checks it (`check`: what to look for).
    WaitingForAcceptance { mode: String, check: String },
}

/// The escape of a step, as the description states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Escape {
    pub leaves: String,
    pub hardware: Option<String>,
    /// The way out: a step to redo or a transition, with its text when it has one.
    pub way_out: String,
    pub text: Option<String>,
}

/// Why a run stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stopped {
    pub step: Option<String>,
    pub why: String,
    pub escape: Option<Box<Escape>>,
    /// A write may have reached the device.
    pub may_have_changed: bool,
}

impl fmt::Display for Stopped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.step {
            Some(s) => write!(f, "stopped in step {s}: {}", self.why)?,
            None => write!(f, "stopped before any step: {}", self.why)?,
        }
        if let Some(e) = &self.escape {
            write!(f, "\n  it leaves: {}", e.leaves)?;
            if let Some(h) = &e.hardware {
                write!(f, " (needs {h})")?;
            }
            write!(f, "\n  the way out: {}", e.way_out)?;
            if let Some(t) = &e.text {
                write!(f, "\n  {t}")?;
            }
        }
        Ok(())
    }
}

impl std::error::Error for Stopped {}

/// The plan's fields the executor reads (the plan is the record's JSON).
#[derive(Debug, Clone, Deserialize)]
struct PlanView {
    status: String,
    steps: Vec<PlanStep>,
    #[serde(default)]
    choices: Vec<PlanChoice>,
}

#[derive(Debug, Clone, Deserialize)]
struct PlanChoice {
    choice: String,
    value: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PlanStep {
    id: String,
    mode: String,
    writes: bool,
    tried: bool,
}

/// Where the device is, as far as the executor knows.
enum Here<F: Transport, R: Transport> {
    Unknown,
    Fastboot {
        mode: String,
        s: Session<F>,
        used: bool,
    },
    Ram {
        mode: String,
        w: Box<Writer<R>>,
        armed: bool,
        cycled: bool,
    },
    Other(String),
}

impl<F: Transport, R: Transport> Here<F, R> {
    fn mode(&self) -> Option<&str> {
        match self {
            Here::Unknown => None,
            Here::Fastboot { mode, .. } | Here::Ram { mode, .. } | Here::Other(mode) => Some(mode),
        }
    }
}

const MAX_HOPS: usize = 8;

/// Runs (or resumes) the plan of `record`. `plan` must be the record's plan,
/// byte for byte.
pub async fn run<E: InstallEdge, S: ArtifactSource>(
    job: &Job<'_>,
    plan: &str,
    record: &mut Record,
    artifacts: &mut Artifacts<S>,
    edge: &mut E,
) -> Result<Outcome, Stopped> {
    let mut x = Exec {
        d: job.description,
        job,
        record,
        artifacts,
        edge,
        here: Here::Unknown,
        new_table: None,
        disk: None,
        choices: BTreeMap::new(),
    };
    let r = x.run(plan).await;
    if r.is_err() {
        x.leave_ram_disarmed().await;
    }
    r
}

struct Exec<'a, E: InstallEdge, S: ArtifactSource> {
    d: &'a Description,
    job: &'a Job<'a>,
    record: &'a mut Record,
    artifacts: &'a mut Artifacts<S>,
    edge: &'a mut E,
    here: Here<E::Fb, E::Ram>,
    /// The new table and the backed-up primary it was built from.
    new_table: Option<(Built, Vec<u8>)>,
    /// The RAM environment's disk the table is on, and its sector size and size.
    disk: Option<(u32, u64, u64)>,
    /// The plan's choices (a partition rule's `when` reads them).
    choices: BTreeMap<String, String>,
}

fn stop(why: impl Into<String>) -> Stopped {
    Stopped {
        step: None,
        why: why.into(),
        escape: None,
        may_have_changed: false,
    }
}

impl<E: InstallEdge, S: ArtifactSource> Exec<'_, E, S> {
    fn step(&self, id: &str) -> Result<&Step, Stopped> {
        self.d
            .steps
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| stop(format!("the plan names step {id}, which the description doesn't have")))
    }

    fn mode(&self, id: &str) -> Result<&Mode, Stopped> {
        self.d
            .modes
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| stop(format!("the description has no mode {id}")))
    }

    fn escape(&self, _st: &Step, case: &EscapeCase) -> Escape {
        escape_in(self.d, case)
    }

    /// Stops in `st` with its failure escape (or interruption escape).
    fn fail(&self, st: &Step, why: impl Into<String>, may_have_changed: bool, interrupted: bool) -> Stopped {
        fail_in(self.d, st, why, may_have_changed, interrupted)
    }

    async fn save(&mut self, st: Option<&Step>) -> Result<(), Stopped> {
        self.edge.save(self.record).await.map_err(|e| {
            let why = format!("the install record couldn't be saved ({e}): stopped; resume once it can be");
            match st {
                Some(st) => self.fail(st, why, false, true),
                None => stop(why),
            }
        })
    }

    async fn run(&mut self, plan: &str) -> Result<Outcome, Stopped> {
        // The record, the plan, the description.
        if plan != self.record.plan || sha256_hex(plan.as_bytes()) != self.record.plan_sha256 {
            return Err(stop("this isn't the plan the record holds: a resume runs only the plan that was confirmed"));
        }
        if self.record.description.id != self.d.id || self.record.description.sha256 != self.job.description_sha256 {
            return Err(stop(format!(
                "the record was made with description {} {}, not this one ({} {})",
                self.record.description.id, self.record.description.sha256, self.d.id, self.job.description_sha256
            )));
        }
        let pv: PlanView = serde_json::from_str(plan).map_err(|e| stop(format!("the plan: {e}")))?;
        match pv.status.as_str() {
            "ready" => {}
            "developer-only" if self.job.developer => {}
            "developer-only" => return Err(stop("the plan has untried write steps: only a developer who asks for them runs it")),
            s => return Err(stop(format!("the plan is {s}: nothing is run"))),
        }
        self.choices = pv.choices.iter().map(|c| (c.choice.clone(), c.value.clone())).collect();
        let order = self.order(&pv)?;
        // The final restart is saved only after every step ran (a step the
        // run went on past stays failed): a resume goes straight to it.
        if self.record.final_restart.is_some() {
            return self.finish().await;
        }
        self.check_artifacts(&order).await?;
        for id in &order {
            let ps = pv.steps.iter().find(|s| s.id == *id).unwrap().clone();
            let st = self.step(id)?.clone();
            if ps.mode != st.mode {
                return Err(stop(format!("the plan runs {id} in {}, the description in {}", ps.mode, st.mode)));
            }
            if self.record.state(id) == StepState::Done {
                continue;
            }
            if ps.writes && !ps.tried && !self.job.developer {
                return Err(self.fail(&st, "an untried write step: only a developer who asks for it runs it", false, false));
            }
            self.run_step(&st).await?;
            if st.safe_stop && self.edge.cancel_requested() {
                self.leave_ram_disarmed().await;
                return Err(Stopped {
                    step: Some(st.id.clone()),
                    why: "cancelled after this step, as asked".into(),
                    escape: Some(Box::new(self.escape(&st, &st.escape.interruption))),
                    may_have_changed: false,
                });
            }
        }
        self.finish().await
    }

    /// The plan's steps, each after the steps it names that the plan has
    /// (the plan's order otherwise).
    fn order(&self, pv: &PlanView) -> Result<Vec<String>, Stopped> {
        let ids: Vec<String> = pv.steps.iter().map(|s| s.id.clone()).collect();
        let mut out: Vec<String> = vec![];
        let mut left: VecDeque<String> = ids.iter().cloned().collect();
        let mut stuck = 0;
        while let Some(id) = left.pop_front() {
            let st = self.step(&id)?;
            if st.after.iter().all(|a| !ids.contains(a) || out.contains(a)) {
                out.push(id);
                stuck = 0;
            } else {
                left.push_back(id);
                stuck += 1;
                if stuck > left.len() {
                    return Err(stop("the plan's steps wait for each other"));
                }
            }
        }
        Ok(out)
    }

    /// Every artifact a step still to run uses: the one the record holds,
    /// read whole and checked, before anything is sent to the device.
    async fn check_artifacts(&mut self, order: &[String]) -> Result<(), Stopped> {
        let mut roles = BTreeSet::new();
        for id in order {
            if self.record.state(id) == StepState::Done {
                continue;
            }
            if let Some(r) = self.step(id)?.params.get("artifact").and_then(|v| v.as_str()) {
                roles.insert(r.to_string());
            }
        }
        for role in roles {
            let a = self.artifacts.artifact(&role).map_err(|e| stop(e.to_string()))?;
            match self.record.artifacts.iter().find(|r| r.role == role) {
                Some(r) if *r == a => {}
                Some(r) => {
                    return Err(stop(format!(
                        "the release's {role} is {} ({} bytes), the record's {} ({} bytes): not the confirmed artifact",
                        a.sha256, a.size, r.sha256, r.size
                    )));
                }
                None => return Err(stop(format!("the record has no {role}: it wasn't confirmed"))),
            }
            let edge = &mut *self.edge;
            self.artifacts
                .verify(&role, &mut |n| edge.progress(&format!("check {role}"), n, a.size))
                .await
                .map_err(|e| stop(e.to_string()))?;
        }
        Ok(())
    }

    fn now(&mut self) -> Option<String> {
        self.edge.now()
    }

    fn outcome(&mut self, result: Result_, evidence: Evidence, message: Option<String>) -> Done {
        Done {
            result,
            evidence,
            message,
            at: self.now(),
        }
    }

    /// Records `st` as found done (no intent of the installer's ran it now).
    async fn record_found_done(&mut self, st: &Step, why: &str) -> Result<(), Stopped> {
        if self.record.open_step() != Some(st.id.as_str()) {
            let at = self.now();
            self.record
                .begin(&st.id, &format!("check whether {} is done", st.id), at)
                .map_err(|e| self.fail(st, e, false, false))?;
        }
        let mut ev = Evidence::new();
        ev.insert("found-done".into(), why.into());
        let o = self.outcome(Result_::Done, ev, None);
        self.record.finish(&st.id, o).map_err(|e| self.fail(st, e, false, false))?;
        self.save(Some(st)).await
    }

    async fn run_step(&mut self, st: &Step) -> Result<(), Stopped> {
        let open = self.record.open_step() == Some(st.id.as_str());
        let decide_by_done = st.kind == "write-boot-loader";
        if open && !decide_by_done {
            // Cut while it ran: closed as interrupted, then run again whole.
            let o = self.outcome(Result_::Interrupted, Evidence::new(), Some("cut while it ran; run again whole".into()));
            self.record.finish(&st.id, o).map_err(|e| self.fail(st, e, false, true))?;
            self.save(Some(st)).await?;
        } else if self.done(st, None).await? == Truth::True {
            return self.record_found_done(st, "its done conditions hold").await;
        }
        if st.kind == "back-up" {
            return self.back_up(st).await;
        }
        let actual = self.move_to(&st.mode, Some(st)).await?;
        if !open || decide_by_done {
            match self.done(st, None).await? {
                Truth::True => return self.record_found_done(st, &format!("its done conditions hold in mode {actual}")).await,
                _ if actual != st.mode => {
                    return Err(self.fail(st, format!("the device came up in mode {actual}, not {}", st.mode), false, open));
                }
                _ => {}
            }
        }
        if actual != st.mode {
            return Err(self.fail(st, format!("the device came up in mode {actual}, not {}", st.mode), false, open));
        }
        // The RAM environment's writes are armed before the intent: a range
        // it refuses stops the run before anything is written.
        if matches!(self.here, Here::Ram { .. }) {
            self.ensure_armed(st).await?;
        }
        if open && decide_by_done {
            let o = self.outcome(Result_::Interrupted, Evidence::new(), Some("cut while it ran; not done: run again".into()));
            self.record.finish(&st.id, o).map_err(|e| self.fail(st, e, false, true))?;
        }
        // The intent, saved before anything is written.
        let mut summary = self.d.texts.steps.get(&st.id).cloned().unwrap_or_else(|| st.kind.clone());
        if let Some(role) = st.params.get("artifact").and_then(|v| v.as_str())
            && let Some(a) = self.record.artifacts.iter().find(|a| a.role == role)
        {
            summary += &format!(" [{role} sha256 {}]", a.sha256);
        }
        let at = self.now();
        self.record.begin(&st.id, &summary, at).map_err(|e| self.fail(st, e, false, false))?;
        self.save(Some(st)).await?;
        self.edge.note(&format!("{}: {summary}", st.id));

        let again = open
            || matches!(
                self.record
                    .journal
                    .iter()
                    .rev()
                    .find(|e| e.step == st.id && e.outcome.is_some())
                    .and_then(|e| e.outcome.as_ref())
                    .map(|o| o.result),
                Some(Result_::Failed | Result_::Interrupted)
            );
        let ev = match self.op(st, again).await {
            Ok(ev) => ev,
            Err(e) if e.goes_on => {
                // Refused before anything was written: recorded, and the run
                // goes on past the step.
                let o = self.outcome(Result_::Failed, Evidence::new(), Some(e.why.clone()));
                self.record.finish(&st.id, o).map_err(|x| self.fail(st, x, false, false))?;
                self.save(Some(st)).await?;
                self.edge.note(&format!("{}: {}; going on", st.id, e.why));
                return Ok(());
            }
            Err(e) => {
                let o = self.outcome(Result_::Failed, Evidence::new(), Some(e.why.clone()));
                let _ = self.record.finish(&st.id, o);
                let _ = self.edge.save(self.record).await;
                return Err(self.fail(st, e.why, e.may_have_changed, false));
            }
        };
        match self.done(st, Some(&st.id)).await {
            Ok(Truth::True) => {}
            r => {
                let why = match r {
                    Err(s) => s.why,
                    Ok(t) => format!("the step ran, but its done conditions are {t:?}"),
                };
                let o = self.outcome(Result_::Failed, ev, Some(why.clone()));
                let _ = self.record.finish(&st.id, o);
                let _ = self.edge.save(self.record).await;
                return Err(self.fail(st, why, true, false));
            }
        }
        let o = self.outcome(Result_::Done, ev, None);
        self.record.finish(&st.id, o).map_err(|e| self.fail(st, e, true, true))?;
        self.save(Some(st)).await
    }

    async fn back_up(&mut self, st: &Step) -> Result<(), Stopped> {
        let at = self.now();
        self.record
            .begin(&st.id, &self.d.texts.steps.get(&st.id).cloned().unwrap_or_default(), at)
            .map_err(|e| self.fail(st, e, false, false))?;
        self.save(Some(st)).await?;
        let b = match self.edge.back_up(st).await {
            Ok(b) if b.manifest_sha256.is_some() => b,
            Ok(_) => return Err(self.fail(st, "the backup isn't verified", false, false)),
            Err(e) => {
                let o = self.outcome(Result_::Failed, Evidence::new(), Some(e.clone()));
                let _ = self.record.finish(&st.id, o);
                let _ = self.edge.save(self.record).await;
                return Err(self.fail(st, e, false, false));
            }
        };
        let mut ev = Evidence::new();
        ev.insert("location".into(), b.location.clone());
        ev.insert("manifest-sha256".into(), b.manifest_sha256.clone().unwrap_or_default());
        self.record.backup = Some(b);
        let o = self.outcome(Result_::Done, ev, None);
        self.record.finish(&st.id, o).map_err(|e| self.fail(st, e, false, true))?;
        // The emergency mode restarts the device into its previous system.
        self.here = Here::Unknown;
        self.save(Some(st)).await
    }

    // ---- done ----------------------------------------------------------------

    /// The installer's own facts: the backup, and the steps done (with
    /// `ran`, the step whose op just succeeded).
    fn installer_facts(&self, ran: Option<&str>) -> InstallerFacts {
        let mut steps_done = self.record.done().unwrap_or_default();
        steps_done.extend(ran.map(String::from));
        InstallerFacts {
            backup_verified: self.record.backup.as_ref().is_some_and(|b| b.manifest_sha256.is_some()),
            steps_done,
        }
    }

    /// One condition, in the mode the device is in (or the installer's own).
    async fn condition(&mut self, c: &Condition, st: &Step, ran: Option<&str>) -> Result<Truth, Stopped> {
        let mode_id = c.mode.clone().unwrap_or_else(|| st.mode.clone());
        if mode_id == INSTALLER_MODE {
            let s = Snapshot {
                mode: INSTALLER_MODE.into(),
                installer: Some(self.installer_facts(ran)),
                ..Default::default()
            };
            return Ok(test(c, &s.observe(&c.key, INSTALLER_MODE)));
        }
        let kind = self.mode(&mode_id)?.kind.clone();
        let mut snap = Snapshot {
            mode: kind.clone(),
            ..Default::default()
        };
        // Read only in the condition's own mode: another boot loader on the
        // same USB ids answers for itself, not for the one the condition names.
        let same_kind = |m: &str| m == mode_id;
        let in_fb = matches!(&self.here, Here::Fastboot { mode, .. } if same_kind(mode));
        let in_ram = matches!(&self.here, Here::Ram { mode, .. } if same_kind(mode));
        if in_fb {
            let mut vars = BTreeMap::new();
            if let Some(v) = c.key.strip_prefix("fastboot.var.") {
                let Here::Fastboot { s, used, .. } = &mut self.here else { unreachable!() };
                let val = if v == "product" {
                    s.product().map(String::from)
                } else {
                    *used = true;
                    match s.getvar(v).await {
                        Ok(x) => x,
                        Err(e) => return Err(self.fail(st, format!("reading {v}: {e}"), false, false)),
                    }
                };
                vars.insert(v.to_string(), val);
            }
            snap.fastboot = Some(VarFacts { variables: vars });
        } else if in_ram {
            // A table that doesn't read (a primary cut while it was written)
            // holds none of the partitions a condition asks for.
            let gpt = match self.live_gpt(st).await {
                Ok(g) => g,
                Err(s) => {
                    self.edge.note(&s.why);
                    Gpt {
                        sector: 512,
                        primary: vec![],
                        entries: 0,
                        backup_header: 0,
                        parts: vec![],
                    }
                }
            };
            snap.storage = Some(Storage {
                names: "maker".into(),
                disks: vec![crate::facts::Disk {
                    index: 0,
                    kind: "internal".into(),
                    size: self.disk.map(|d| d.2).unwrap_or(0),
                    table: Some("gpt".into()),
                    sector_size: Some(gpt.sector),
                    partitions: gpt
                        .parts
                        .iter()
                        .map(|p| crate::facts::Partition {
                            index: p.slot,
                            name: Some(p.name.clone()),
                            start: Some(p.first * gpt.sector),
                            size: (p.last - p.first + 1) * gpt.sector,
                            ..Default::default()
                        })
                        .collect(),
                    ..Default::default()
                }],
                installer_disk: None,
            });
        } else {
            return Ok(Truth::Undetermined);
        }
        let o = snap.observe(&c.key, &kind);
        Ok(if o == Obs::Unknown { Truth::Undetermined } else { test(c, &o) })
    }

    /// The step's `done`: all its conditions. With `ran`, after the op: a
    /// condition read in another mode moves the device there to read it.
    async fn done(&mut self, st: &Step, ran: Option<&str>) -> Result<Truth, Stopped> {
        let mut all = Truth::True;
        for c in &st.done {
            let mut t = self.condition(c, st, ran).await?;
            if t == Truth::Undetermined
                && ran.is_some()
                && let Some(m) = c.mode.clone().filter(|m| m != INSTALLER_MODE)
            {
                let reached = self.move_to(&m, Some(st)).await?;
                t = self.condition(c, st, ran).await?;
                if t == Truth::Undetermined {
                    self.edge.note(&format!("{}: the device came up in mode {reached}, not {m}", st.id));
                    t = Truth::False;
                }
            }
            match t {
                Truth::False => return Ok(Truth::False),
                Truth::Undetermined => all = Truth::Undetermined,
                Truth::True => {}
            }
        }
        Ok(all)
    }

    // ---- moving ----------------------------------------------------------------

    /// A route of transitions from `from` to `to`: the installer's own first,
    /// then ones by hand that need nothing but hands (and the cover).
    fn route(&self, from: &str, to: &str) -> Option<Vec<String>> {
        for allow_hand in [false, true] {
            let ok =
                |t: &crate::format1::Transition| t.by == "installer" || (allow_hand && t.by == "hand" && t.needs.iter().all(|n| n == "hands" || n == "cover"));
            let mut prev: BTreeMap<String, String> = BTreeMap::new();
            let mut q = VecDeque::from([from.to_string()]);
            let mut seen = BTreeSet::from([from.to_string()]);
            while let Some(m) = q.pop_front() {
                if m == to {
                    let mut path = vec![];
                    let mut at = to.to_string();
                    while at != from {
                        let tid = prev[&at].clone();
                        at = self.d.transitions.iter().find(|t| t.id == tid).unwrap().from.clone();
                        path.push(tid);
                    }
                    path.reverse();
                    return Some(path);
                }
                for t in self.d.transitions.iter().filter(|t| t.from == m && ok(t)) {
                    if seen.insert(t.to.clone()) {
                        prev.insert(t.to.clone(), t.id.clone());
                        q.push_back(t.to.clone());
                    }
                }
            }
        }
        None
    }

    /// Moves the device into `target`. Returns the mode it is in: `target`,
    /// or another mode of the same kind it came up in instead (the caller
    /// decides; a boot loader that came back as the old one is the case).
    async fn move_to(&mut self, target: &str, st: Option<&Step>) -> Result<String, Stopped> {
        let fail = |x: &Self, why: String| match st {
            Some(st) => x.fail(st, why, false, false),
            None => stop(why),
        };
        let target_kind = self.mode(target)?.kind.clone();
        for _ in 0..MAX_HOPS {
            let cur = match self.here.mode() {
                None => {
                    let text = self.d.texts.modes.get(target).cloned();
                    let m = self.mode(target)?.clone();
                    let link = self.edge.reach(&m, Reach::Already { text }).await.map_err(|e| fail(self, e))?;
                    self.adopt(link, st).await?;
                    continue;
                }
                Some(c) => c.to_string(),
            };
            if cur == target {
                return Ok(cur);
            }
            if self.mode(&cur)?.kind == target_kind && self.here_fresh() {
                // Came up in a sibling mode (another boot loader on the same
                // USB ids): the caller decides.
                return Ok(cur);
            }
            let path = self
                .route(&cur, target)
                .ok_or_else(|| fail(self, format!("the description has no way from mode {cur} to {target}")))?;
            let tid = path[0].clone();
            self.hop(&tid, st).await?;
        }
        Err(fail(self, format!("mode {target} wasn't reached in {MAX_HOPS} restarts")))
    }

    /// True right after a mode was entered (nothing done in it yet).
    fn here_fresh(&self) -> bool {
        match &self.here {
            Here::Fastboot { used, .. } => !used,
            Here::Ram { armed, .. } => !armed,
            _ => true,
        }
    }

    /// One transition.
    async fn hop(&mut self, tid: &str, st: Option<&Step>) -> Result<(), Stopped> {
        let fail = |x: &Self, why: String| match st {
            Some(st) => x.fail(st, why, false, false),
            None => stop(why),
        };
        let t = self.d.transitions.iter().find(|t| t.id == tid).unwrap().clone();
        let to = self.mode(&t.to)?.clone();
        let from = self.mode(&t.from)?.clone();
        let how = if t.by == "hand" {
            Reach::AskPerson {
                transition: Some(t.id.clone()),
                text: self
                    .d
                    .texts
                    .transitions
                    .get(&t.id)
                    .or_else(|| self.d.texts.modes.get(&t.to))
                    .cloned()
                    .unwrap_or_else(|| format!("bring the device into {}", t.to)),
            }
        } else {
            match std::mem::replace(&mut self.here, Here::Unknown) {
                Here::Fastboot { mode, mut s, .. } => {
                    let r = if to.kind == "fastboot" {
                        s.reboot_bootloader().await
                    } else if to.kind == crate::ram_backup::RAM_MODE_KIND {
                        if !from.departures.iter().any(|x| x.kind == "oem-reboot-recovery") {
                            return Err(fail(self, format!("mode {mode} has no way the installer knows into {}", to.id)));
                        }
                        // In the same session as the commands before it (the
                        // inspect's getvars, a flash): the boot loader's
                        // equal-length framing answers later commands.
                        let rules = self.rules(&mode).map_err(|e| fail(self, e))?;
                        match rules.check_oem(crate::write_rules::OEM_REBOOT_RECOVERY_VERB) {
                            Ok(()) => s.oem(&rules.rules, crate::write_rules::OEM_REBOOT_RECOVERY_VERB).await,
                            Err(e) => Err(e),
                        }
                    } else {
                        s.reboot().await
                    };
                    r.map_err(|e| fail(self, format!("{}: {e}", t.id)))?;
                    Reach::Sent {
                        transition: Some(t.id.clone()),
                    }
                }
                Here::Ram { mut w, .. } => {
                    let to_boot_loader = to.kind == "fastboot";
                    let _ = w.disarm().await;
                    w.reboot(if to_boot_loader { Reboot::BootLoader } else { Reboot::Normal })
                        .await
                        .map_err(|e| fail(self, format!("{}: {e}", t.id)))?;
                    Reach::Sent {
                        transition: Some(t.id.clone()),
                    }
                }
                _ => Reach::Perform {
                    transition: t.id.clone(),
                    text: self.d.texts.modes.get(&t.to).cloned(),
                },
            }
        };
        self.edge.note(&format!("{}: {} -> {}", t.id, t.from, t.to));
        let link = self.edge.reach(&to, how).await.map_err(|e| fail(self, e))?;
        self.adopt(link, st).await
    }

    fn rules(&self, mode: &str) -> Result<ModeRules, String> {
        self.job
            .write_rules
            .get(mode)
            .cloned()
            .ok_or_else(|| format!("no write rules for mode {mode}: nothing is written there"))
    }

    /// Takes what the edge found: a fastboot session (its mode told by the
    /// boot loader's answer), the RAM environment, or a mode named.
    async fn adopt(&mut self, link: Link<E::Fb, E::Ram>, st: Option<&Step>) -> Result<(), Stopped> {
        let fail = |x: &Self, why: String| match st {
            Some(st) => x.fail(st, why, false, false),
            None => stop(why),
        };
        match link {
            Link::Fastboot(mut t) => {
                let table = framing_table(&[self.d]).map_err(|e| fail(self, e.to_string()))?;
                let product = match crate::fastboot::opening(&mut t).await {
                    Ok(Reply::Okay(p)) => Some(p),
                    Ok(Reply::Fail(_)) => None,
                    Ok(r) => return Err(fail(self, format!("the boot loader's opening answered {r:?}"))),
                    Err(e) => return Err(fail(self, busy_hint(&e))),
                };
                let (framing, holding) = choose_framing(&table, product.as_deref()).map_err(|e| fail(self, e.to_string()))?;
                let fb: Vec<&Mode> = self.d.modes.iter().filter(|m| m.kind == "fastboot").collect();
                let mode = match holding.iter().find(|r| r.description == self.d.id) {
                    Some(r) => r.mode.clone(),
                    None if fb.len() == 1 => fb[0].id.clone(),
                    None => {
                        return Err(fail(
                            self,
                            format!("a boot loader that answers product {product:?}, which no mode of the description tells"),
                        ));
                    }
                };
                let s = Session::start(t, framing, product).await.map_err(|e| fail(self, busy_hint(&e)))?;
                self.edge.note(&format!("in mode {mode} ({})", s.framing().name()));
                self.here = Here::Fastboot { mode, s, used: false };
            }
            Link::Ram(t) => {
                let m = self
                    .d
                    .modes
                    .iter()
                    .find(|m| m.kind == crate::ram_backup::RAM_MODE_KIND)
                    .ok_or_else(|| fail(self, "the description has no RAM environment".into()))?
                    .id
                    .clone();
                let mut w = Writer::new(t);
                let l = w.list().await.map_err(|e| fail(self, e.to_string()))?;
                let armed = !matches!(l.writes, Writes::Off);
                let cycled = matches!(&self.here, Here::Ram { cycled: true, .. });
                self.edge.note(&format!("in mode {m} ({} disks, writes {:?})", l.listing.disks.len(), l.writes));
                self.here = Here::Ram {
                    mode: m,
                    w: Box::new(w),
                    armed,
                    cycled,
                };
                if armed {
                    self.disk = None;
                }
            }
            Link::Elsewhere(m) => {
                self.mode(&m)?;
                self.here = Here::Other(m);
            }
        }
        Ok(())
    }

    // ---- the RAM environment ------------------------------------------------------

    /// The RAM environment's disk the partition table is on: the one whose
    /// size is the backed-up table's disk.
    async fn ram_disk(&mut self, st: &Step) -> Result<(u32, u64, u64), Stopped> {
        if let Some(d) = self.disk {
            return Ok(d);
        }
        let (src, sector) = self.source_table(st).await?;
        let disk_sectors = crate::table::le64(&src[sector as usize..], 32) + 1;
        let Here::Ram { w, .. } = &mut self.here else {
            return Err(self.fail(st, "not in the RAM environment", false, false));
        };
        let l = w.list().await.map_err(|e| self.fail(st, e.to_string(), false, false))?;
        let hits: Vec<_> = l
            .listing
            .disks
            .iter()
            .filter(|d| d.sector as u64 == sector && d.size == disk_sectors * sector)
            .collect();
        let d = match hits.as_slice() {
            [d] => (d.index, sector, d.size),
            _ => {
                return Err(self.fail(
                    st,
                    format!("{} of the RAM environment's disks have the backed-up table's size", hits.len()),
                    false,
                    false,
                ));
            }
        };
        self.disk = Some(d);
        Ok(d)
    }

    /// The backed-up primary table and its sector size (the layout's).
    async fn source_table(&mut self, st: &Step) -> Result<(Vec<u8>, u64), Stopped> {
        if let Some((_, src)) = self.new_table.clone() {
            let s = self.layout_sector(st).await?;
            return Ok((src, s));
        }
        let s = self.layout_sector(st).await?;
        let head = self
            .edge
            .backup_bytes(0, 2 * s as usize)
            .await
            .map_err(|e| self.fail(st, format!("the backup: {e}"), false, false))?;
        let len = ops::primary_len(&head, s).map_err(|e| self.fail(st, format!("the backed-up table: {e}"), false, false))?;
        let src = self
            .edge
            .backup_bytes(0, len as usize)
            .await
            .map_err(|e| self.fail(st, format!("the backup: {e}"), false, false))?;
        Ok((src, s))
    }

    async fn layout_bytes(&mut self, st: &Step) -> Result<Vec<u8>, Stopped> {
        let role = "partition-layout";
        let a = self.artifacts.artifact(role).map_err(|e| self.fail(st, e.to_string(), false, false))?;
        let d = self.d;
        let mut r = self.artifacts.reader(role).await.map_err(|e| fail_in(d, st, e.to_string(), false, false))?;
        let mut b = vec![0u8; a.size as usize];
        crate::table::AsyncReadAt::read_at(&mut r, 0, &mut b)
            .await
            .map_err(|e| fail_in(d, st, e, false, false))?;
        r.finish().map_err(|e| fail_in(d, st, e.to_string(), false, false))?;
        Ok(b)
    }

    async fn layout_sector(&mut self, st: &Step) -> Result<u64, Stopped> {
        let b = self.layout_bytes(st).await?;
        let r: crate::gpt_build::Recipe = serde_json::from_slice(&b).map_err(|e| self.fail(st, format!("the partition layout: {e}"), false, false))?;
        Ok(r.sector)
    }

    /// The new table (built once per run).
    async fn new_table(&mut self, table_step: &Step) -> Result<Built, Stopped> {
        if let Some((b, _)) = &self.new_table {
            return Ok(b.clone());
        }
        let (src, _) = self.source_table(table_step).await?;
        let disk_bytes = self.ram_disk(table_step).await?.2;
        let layout = self.layout_bytes(table_step).await?;
        let b = ops::partition_table::layout(&src, disk_bytes, &layout, table_step).map_err(|e| self.fail(table_step, e.why, false, false))?;
        self.new_table = Some((b.clone(), src));
        Ok(b)
    }

    async fn gpt_from_backup(&mut self, st: &Step) -> Result<Gpt, Stopped> {
        let (disk, sector, disk_bytes) = self.ram_disk(st).await?;
        let mbr = self
            .edge
            .backup_bytes(0, sector as usize)
            .await
            .map_err(|e| self.fail(st, format!("the backup: {e}"), false, false))?;
        let Here::Ram { w, .. } = &mut self.here else {
            return Err(self.fail(st, "not in the RAM environment", false, false));
        };
        ops::gpt_from_backup(w, disk, sector, disk_bytes, &mbr)
            .await
            .map_err(|e| self.fail(st, e, false, false))
    }

    async fn live_gpt(&mut self, st: &Step) -> Result<Gpt, Stopped> {
        let (disk, sector, _) = self.ram_disk(st).await?;
        let Here::Ram { w, .. } = &mut self.here else {
            return Err(self.fail(st, "not in the RAM environment", false, false));
        };
        ops::read_gpt(w, disk, sector)
            .await
            .map_err(|e| self.fail(st, format!("the partition table on the device: {e}"), false, false))
    }

    /// Arms every range the plan's remaining steps in this mode write: once
    /// per start of the environment. Found armed, the environment is
    /// restarted through the boot loader first (once).
    async fn ensure_armed(&mut self, st: &Step) -> Result<(), Stopped> {
        let (armed, cycled) = match &self.here {
            Here::Ram { armed, cycled, .. } => (*armed, *cycled),
            _ => return Err(self.fail(st, "not in the RAM environment", false, false)),
        };
        if armed && self.here_armed_by_us() {
            return Ok(());
        }
        let mut used_up = armed;
        if !armed {
            let ranges = self.ranges(st).await?;
            let d = self.d;
            let Here::Ram { w, armed, .. } = &mut self.here else { unreachable!() };
            match w.arm(&ranges).await {
                Ok(n) => {
                    *armed = true;
                    self.edge.note(&format!("armed {n} ranges"));
                    return Ok(());
                }
                Err(RamError::Status {
                    status: STATUS_ALREADY_ARMED, ..
                }) => used_up = true,
                Err(e) => return Err(fail_in(d, st, format!("arming the writes: {e}"), false, false)),
            }
        }
        debug_assert!(used_up);
        if cycled {
            return Err(self.fail(st, "the RAM environment can't be armed even after a restart", false, false));
        }
        // An earlier run armed this start of the environment: restart it
        // through the boot loader.
        self.edge.note("the RAM environment was armed by an earlier run: restarting it");
        let mode = self.here.mode().unwrap().to_string();
        let back = self
            .d
            .transitions
            .iter()
            .find(|t| t.from == mode && t.by == "installer" && self.d.modes.iter().any(|m| m.id == t.to && m.kind == "fastboot"))
            .map(|t| t.id.clone())
            .ok_or_else(|| self.fail(st, "no way back to the boot loader to restart the RAM environment", false, false))?;
        self.hop(&back, Some(st)).await?;
        self.move_to(&mode, Some(st)).await?;
        if let Here::Ram { cycled, .. } = &mut self.here {
            *cycled = true;
        }
        Box::pin(self.ensure_armed(st)).await
    }

    fn here_armed_by_us(&self) -> bool {
        matches!(&self.here, Here::Ram { w, armed: true, .. } if !w.armed().is_empty())
    }

    /// The ranges to arm: for every step still to run in this mode, the
    /// partition extents it writes in the table that will be there (the new
    /// one while the table step is still to come), and both table copies for
    /// the table and slot steps. Each must be a partition the step writes
    /// and none never-write.
    async fn ranges(&mut self, st: &Step) -> Result<Vec<Range>, Stopped> {
        let mode = self.here.mode().unwrap_or("").to_string();
        let ids = self.record.plan_steps().map_err(|e| self.fail(st, e, false, false))?;
        let pending: Vec<Step> = ids
            .iter()
            .filter(|id| self.record.state(id) != StepState::Done)
            .filter_map(|id| self.d.steps.iter().find(|s| s.id == *id && s.mode == mode))
            .cloned()
            .collect();
        let (disk, sector, _) = self.ram_disk(st).await?;
        let table_pending = pending.iter().find(|s| s.kind == "write-partition-table").cloned();
        // The table as it will be: its parts, and both copies' places.
        let (parts, copies): (Gpt, Vec<(u64, u64)>) = match &table_pending {
            Some(ts) => {
                let b = self.new_table(ts).await?;
                let g = ops::parse_gpt(&b.primary, sector).map_err(|e| self.fail(st, e, false, false))?;
                (g, vec![(0, b.primary.len() as u64), (b.backup_offset(), b.backup.len() as u64)])
            }
            None => {
                let g = match self.live_gpt(st).await {
                    Ok(g) => g,
                    // A primary torn by a cut slot-flag write: the places
                    // from the backup copy (the step rebuilds the primary).
                    Err(s) => self.gpt_from_backup(st).await.map_err(|b| Stopped {
                        why: format!("{}; its backup copy: {}", s.why, b.why),
                        ..s
                    })?,
                };
                let arr = g.primary.len() as u64 - 2 * sector;
                let backup_at = (g.backup_header * sector).saturating_sub(arr);
                let copies = vec![(0, g.primary.len() as u64), (backup_at, arr + sector)];
                (g, copies)
            }
        };
        let never = ops::never_write(self.d, &self.choices);
        let mut out: BTreeSet<Range> = BTreeSet::new();
        for p in &pending {
            match p.kind.as_str() {
                "write-partition-table" | "set-boot-slot" => {
                    for (o, l) in &copies {
                        out.insert(Range { disk, offset: *o, length: *l });
                    }
                }
                "write-image" | "write-base" | "copy-partition" | "patch-boot-image" => {
                    let name = ops::param(p, "partition").map_err(|e| self.fail(st, e.why, false, false))?;
                    if !p.writes.iter().any(|w| w == name) || never.refuses(name) {
                        return Err(self.fail(p, format!("{name} isn't a partition step {} may write", p.id), false, false));
                    }
                    let (o, l) = parts
                        .extent(name)
                        .ok_or_else(|| self.fail(p, format!("the partition table has no {name}"), false, false))?;
                    out.insert(Range { disk, offset: o, length: l });
                }
                _ => {}
            }
        }
        Ok(out.into_iter().collect())
    }

    // ---- ops --------------------------------------------------------------------

    async fn op(&mut self, st: &Step, resumed: bool) -> Result<Evidence, OpError> {
        let role = st.params.get("artifact").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let id = st.id.clone();
        let mode = self.here.mode().unwrap_or("").to_string();
        if let Here::Fastboot { s, used, .. } = &mut self.here {
            *used = true;
            let part = ops::param(st, "partition")?.to_string();
            let rules = self
                .job
                .write_rules
                .get(&mode)
                .ok_or_else(|| OpError::before(format!("no write rules for mode {mode}: nothing is written there")))?;
            rules.check(&part).map_err(|e| OpError::before(e.to_string()))?;
            let rules = &rules.rules;
            return match st.kind.as_str() {
                "write-boot-loader" => ops::boot_loader::run(s, rules, &part, &role, self.artifacts).await,
                "write-image" => ops::write_image::flash(s, rules, &part, &role, self.artifacts).await,
                k => Err(OpError::before(format!("step kind {k} isn't built for a boot loader's fastboot"))),
            };
        }
        if !matches!(self.here, Here::Ram { .. }) {
            return Err(OpError::before(format!("step kind {} isn't built for mode {mode}", st.kind)));
        }
        self.ensure_armed(st).await.map_err(|s| OpError::before(s.why))?;
        let (disk, sector, disk_bytes) = self.ram_disk(st).await.map_err(|s| OpError::before(s.why))?;
        let table = if st.kind == "write-partition-table" {
            Some(self.new_table(st).await.map_err(|s| OpError::before(s.why))?)
        } else {
            None
        };
        let gpt = if matches!(st.kind.as_str(), "write-image" | "write-base" | "copy-partition" | "patch-boot-image") {
            Some(self.live_gpt(st).await.map_err(|s| OpError::before(s.why))?)
        } else {
            None
        };
        let src = self.new_table.as_ref().map(|(_, s)| s.clone()).unwrap_or_default();
        // The backed-up disk's sector 0: the protective MBR every table copy
        // the installer writes carries (a slot step's torn primary is rebuilt with it).
        let mbr = if st.kind == "set-boot-slot" && resumed {
            self.edge
                .backup_bytes(0, sector as usize)
                .await
                .map_err(|e| OpError::before(format!("the backup: {e}")))?
        } else {
            vec![]
        };
        // A boot image's original: kept on the computer before the first
        // write, read from there on a resume (the partition may be half-written).
        let original = if st.kind == "patch-boot-image" {
            let part = ops::param(st, "partition")?;
            let (start, len) = gpt
                .as_ref()
                .and_then(|g| g.extent(part))
                .ok_or_else(|| OpError::before(format!("the partition table has no {part}")))?;
            let key = format!("{}-{part}-original", st.id);
            match self
                .edge
                .kept(&key)
                .await
                .map_err(|e| OpError::before(format!("the kept original of {part}: {e}")))?
            {
                Some(b) if b.len() as u64 == len => {
                    self.edge.note(&format!("{}: the original of {part} as read before the first write", st.id));
                    Some(b)
                }
                Some(b) => {
                    return Err(OpError::before(format!(
                        "the original of {part} kept on the computer is {} bytes, the partition {len}",
                        b.len()
                    )));
                }
                None => {
                    let Here::Ram { w, .. } = &mut self.here else { unreachable!() };
                    let b = w.read_all(disk, start, len).await.map_err(|e| ops::ram_err(e, false))?;
                    self.edge
                        .keep(&key, &b)
                        .await
                        .map_err(|e| OpError::before(format!("keeping the original of {part} on the computer: {e}")))?;
                    Some(b)
                }
            }
        } else {
            None
        };
        let Here::Ram { w, .. } = &mut self.here else { unreachable!() };
        let max_hash = w.list().await.map_err(|e| ops::ram_err(e, false))?.listing.max_hash;
        let edge = &mut *self.edge;
        let mut progress = |d: u64, t: u64| edge.progress(&id, d, t);
        match st.kind.as_str() {
            "write-partition-table" => ops::partition_table::run(w, disk, table.as_ref().unwrap(), &src, resumed, max_hash).await,
            "set-boot-slot" => {
                let flags = crate::slot_flags::SlotFlags::of(self.d)
                    .map_err(OpError::before)?
                    .ok_or_else(|| OpError::before("the description names no slot-flag scheme (its slot-flags boot setting)"))?;
                ops::set_boot_slot::run(w, disk, sector, disk_bytes, &flags, st, resumed, &mbr, max_hash).await
            }
            "write-image" => {
                let part = ops::param(st, "partition")?;
                ops::write_image::write_ram(w, disk, gpt.as_ref().unwrap(), part, &role, self.artifacts, max_hash, &mut progress).await
            }
            "write-base" => {
                let part = ops::param(st, "partition")?;
                ops::write_base::run(w, disk, gpt.as_ref().unwrap(), part, &role, self.artifacts, max_hash, &mut progress).await
            }
            "patch-boot-image" => {
                let part = ops::param(st, "partition")?;
                let (start, _) = gpt.as_ref().and_then(|g| g.extent(part)).unwrap();
                ops::patch_boot::run(w, disk, part, start, original.as_deref().unwrap(), max_hash, &mut progress).await
            }
            "copy-partition" => {
                let from = ops::param(st, "from")?;
                let part = ops::param(st, "partition")?;
                ops::copy_partition::run(w, disk, gpt.as_ref().unwrap(), from, part, max_hash, &mut progress).await
            }
            k => Err(OpError::before(format!("step kind {k} isn't built for the RAM environment"))),
        }
    }

    // ---- the end ------------------------------------------------------------------

    /// The restart into the installed system, after the last step. Saved in
    /// the record before it is sent and once it was: a resume after it
    /// doesn't touch the device; one cut before it finds the device and
    /// sends it (or, not finding it, waits for the person's check).
    async fn finish(&mut self) -> Result<Outcome, Stopped> {
        if let Some(f) = &self.record.final_restart
            && f.sent
        {
            return Ok(Outcome::WaitingForAcceptance {
                mode: f.mode.clone(),
                check: f.check.clone(),
            });
        }
        // The installer's own restart into a glassine-usb mode, from the
        // mode the last step ran in.
        let last = self
            .record
            .plan_steps()
            .unwrap_or_default()
            .last()
            .and_then(|id| self.d.steps.iter().find(|s| s.id == *id))
            .map(|s| s.mode.clone());
        let from = self.here.mode().map(String::from).or(last);
        let Some(cur) = from else {
            return Ok(Outcome::Complete);
        };
        let Some(t) = self
            .d
            .transitions
            .iter()
            .find(|t| t.from == cur && t.by == "installer" && self.d.modes.iter().any(|m| m.id == t.to && m.kind == "glassine-usb"))
            .cloned()
        else {
            return Ok(Outcome::Complete);
        };
        let to = self.mode(&t.to)?.clone();
        let check = self
            .d
            .texts
            .modes
            .get(&to.id)
            .cloned()
            .unwrap_or_else(|| format!("the device starts {}", to.id));
        if matches!(self.here, Here::Unknown) {
            // A resume after every step was done: the device is where the
            // cut left it.
            let m = self.mode(&cur)?.clone();
            let text = self.d.texts.modes.get(&cur).cloned();
            match self.edge.reach(&m, Reach::Already { text }).await {
                Ok(Link::Ram(r)) => self.adopt(Link::Ram(r), None).await?,
                Ok(_) | Err(_) if self.record.final_restart.is_some() => {
                    self.edge.note(&format!("{}: the device isn't in {cur} any more: it took the restart", t.id));
                    return self.restarted(&t, &to, check).await;
                }
                Ok(_) => {
                    self.edge.note(&format!("every step is done; the device isn't in {cur}: restart it normally"));
                    return Ok(Outcome::WaitingForAcceptance { mode: to.id, check });
                }
                Err(e) => return Err(stop(format!("every step is done, but the device wasn't found for {}: {e}", t.id))),
            }
        }
        if let Here::Ram { w, armed, .. } = &mut self.here {
            w.disarm().await.map_err(|e| stop(format!("disarming at the end: {e}")))?;
            *armed = false;
        }
        let Here::Ram { .. } = self.here else {
            return Ok(Outcome::Complete);
        };
        self.record.final_restart = Some(FinalRestart {
            transition: t.id.clone(),
            mode: to.id.clone(),
            check: check.clone(),
            sent: false,
        });
        self.save(None).await?;
        if let Here::Ram { mut w, .. } = std::mem::replace(&mut self.here, Here::Unknown) {
            w.reboot(Reboot::Normal).await.map_err(|e| stop(format!("{}: {e}", t.id)))?;
        }
        self.restarted(&t, &to, check).await
    }

    async fn restarted(&mut self, t: &crate::format1::Transition, to: &Mode, check: String) -> Result<Outcome, Stopped> {
        self.record.final_restart = Some(FinalRestart {
            transition: t.id.clone(),
            mode: to.id.clone(),
            check: check.clone(),
            sent: true,
        });
        self.save(None).await?;
        self.edge.note(&format!("{}: the device restarts into {}", t.id, to.id));
        // TODO(glassine-usb): a mode that can identify is reached and
        // checked here; GlassineOS isn't recognised over USB yet.
        Ok(Outcome::WaitingForAcceptance { mode: to.id.clone(), check })
    }

    /// Leaves an armed RAM environment disarmed (a run that stopped).
    async fn leave_ram_disarmed(&mut self) {
        if let Here::Ram { w, armed, .. } = &mut self.here
            && *armed
            && !w.armed().is_empty()
            && w.disarm().await.is_ok()
        {
            *armed = false;
        }
    }
}

fn escape_in(d: &Description, case: &EscapeCase) -> Escape {
    let (way_out, text) = match (&case.way_out.step, &case.way_out.transition) {
        (Some(s), _) => (format!("step {s}"), d.texts.steps.get(s).cloned()),
        (_, Some(t)) => (
            format!("transition {t}"),
            d.texts.transitions.get(t).cloned().or_else(|| {
                let to = d.transitions.iter().find(|x| x.id == *t).map(|x| x.to.clone());
                to.and_then(|m| d.texts.modes.get(&m).cloned())
            }),
        ),
        _ => (String::new(), None),
    };
    Escape {
        leaves: case.leaves.clone(),
        hardware: case.hardware.clone(),
        way_out,
        text,
    }
}

fn fail_in(d: &Description, st: &Step, why: impl Into<String>, may_have_changed: bool, interrupted: bool) -> Stopped {
    let case = if interrupted { &st.escape.interruption } else { &st.escape.failure };
    Stopped {
        step: Some(st.id.clone()),
        why: why.into(),
        escape: Some(Box::new(escape_in(d, case))),
        may_have_changed,
    }
}

/// A transport error, with what to do when the interface was busy: a host
/// adb server holds a device's interface it knows.
fn busy_hint(e: &FbError) -> String {
    let t = e.to_string();
    if t.contains("Resource busy") || t.contains("busy") {
        format!("{t}: another program holds the device's USB interface; a running adb server does: stop it with `adb kill-server`")
    } else {
        t
    }
}
