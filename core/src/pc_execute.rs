// SPDX-License-Identifier: GPL-2.0-or-later
//! The executor of a PC's plan (mode `local`): the installer runs on the
//! machine it installs, so its steps read and write the machine's own disks
//! and firmware variables, through a [`Machine`] the edge gives (on Linux
//! the fenced writer crate, in tests a simulated one). The step vocabulary,
//! the install record as the journal, resume and the escapes are the
//! executor's ([`crate::execute`]):
//!
//! 1. a step the record holds as done is skipped; one whose `done`
//!    conditions hold is recorded as found done;
//! 2. the intent is saved before anything is written;
//! 3. the step's op writes, flushes and reads every byte back past the
//!    cache, and compares;
//! 4. only a step whose `done` holds afterwards is recorded done.
//!
//! A step cut while it ran (an intent without an outcome) runs again whole:
//! every write is a whole rewrite of the same bytes. What makes them the
//! same is kept on the computer before the first write: the disk's table as
//! it was, its identity, and the unique GUIDs the edge drew.
//!
//! Every write is checked against the ranges the step may write (the
//! table's two copies, the partition the step names, which must be one of
//! GlassineOS's new ones); a write anywhere else is refused before it is
//! sent. Nothing outside the plan's target disk is written, and nothing on
//! it before the backup (or the person's choice to go without) is recorded.
//! The firmware's variables are written only by `add-boot-entry`: one new
//! `Boot####` (or the one already ours, recognised by the boot partition's
//! unique GUID) and the boot order with ours first and every other entry in
//! its order.

use crate::artifacts::{ArtifactSource, Artifacts};
use crate::efivar;
use crate::execute::{Escape, Outcome, Stopped, escape_in, fail_in};
use crate::facts::{InstallerFacts, Obs, Snapshot, Storage, Truth, test};
use crate::fat;
use crate::format1::{Condition, Description, Step};
use crate::lock::sha256_hex;
use crate::ops::{Evidence, OpError, hex, hex_sha, parse_size};
use crate::pc_boot::{self, BootFiles, Content, KernelSet, Part};
use crate::pc_table::{self, NewPart, NewSize, PcTable, Source};
use crate::plan::{BACKUP_CHOICE, PartAt};
use crate::record::{Backup, Outcome as Done, Record, Result_, StepState};
use crate::table::{self, AsyncReadAt, ReadAt};
use crate::vocab::INSTALLER_MODE;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// How much is written, read and hashed at once.
pub const CHUNK: usize = 16 << 20;

/// The partition types GlassineOS's partitions get (001 FR-012a).
pub const ESP_TYPE: &str = crate::table::ESP_TYPE;
pub const LINUX_DATA_TYPE: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
/// The root partition type of the Discoverable Partitions Specification, by
/// architecture as descriptions name it.
pub fn root_type(arch: &str) -> Option<&'static str> {
    match arch {
        "x86-64" => Some("4f68bce3-e8cd-4db1-96e7-fbcaf984b709"),
        "arm64" => Some("b921b045-1df0-41c3-af44-4c6f280d3fae"),
        _ => None,
    }
}

/// A disk of the machine as the edge lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskId {
    pub index: u32,
    pub size: u64,
    pub sector: u64,
    pub kind: String,
    #[serde(default)]
    pub model: Option<String>,
}

/// The machine the installer runs on: its disks and its firmware's
/// variables (efivarfs files: four attribute bytes, then the value). Reads
/// return what the disk holds, not what a cache holds.
pub trait Machine {
    fn disks(&mut self) -> Result<Vec<DiskId>, String>;
    fn read(&mut self, disk: u32, offset: u64, buf: &mut [u8]) -> Result<(), String>;
    fn write(&mut self, disk: u32, offset: u64, data: &[u8]) -> Result<(), String>;
    /// Makes every write to `disk` durable.
    fn flush(&mut self, disk: u32) -> Result<(), String>;
    /// After a table write: the system rereads the disk's table (best effort).
    fn table_written(&mut self, _disk: u32) {}
    /// The names of the global variables (`Name-<GUID>`).
    fn var_names(&mut self) -> Result<Vec<String>, String>;
    fn var(&mut self, name: &str) -> Result<Option<Vec<u8>>, String>;
    fn set_var(&mut self, name: &str, file: &[u8]) -> Result<(), String>;
}

/// The computer's side: where the record and the backup go, the person.
pub trait PcEdge {
    /// Saves the record atomically, durable before it returns.
    fn save(&mut self, record: &Record) -> Result<(), String>;
    /// Keeps `bytes` under `name` beside the record, durable before it returns.
    fn keep(&mut self, name: &str, bytes: &[u8]) -> Result<(), String>;
    fn kept(&mut self, name: &str) -> Result<Option<Vec<u8>>, String>;
    /// Where the backup goes, as the person chose it; `None` when none was given.
    fn backup_location(&self) -> Option<String>;
    /// One stored file of the backup, written in pieces and made durable at the end.
    fn backup_begin(&mut self, name: &str) -> Result<(), String>;
    fn backup_append(&mut self, bytes: &[u8]) -> Result<(), String>;
    fn backup_end(&mut self) -> Result<(), String>;
    /// Reads a stored file back, as stored (decompressed), in pieces.
    fn backup_read(&mut self, name: &str, each: &mut dyn FnMut(&[u8]) -> Result<(), String>) -> Result<(), String>;
    /// A small file of the backup (the manifest, the variables), whole.
    fn backup_put(&mut self, name: &str, bytes: &[u8]) -> Result<(), String>;
    fn backup_get(&mut self, name: &str) -> Result<Option<Vec<u8>>, String>;
    /// 16 random bytes for a new unique GUID.
    fn random(&mut self) -> [u8; 16];
    fn progress(&mut self, step: &str, done: u64, total: u64);
    fn note(&mut self, line: &str);
    fn cancel_requested(&mut self) -> bool;
    fn now(&mut self) -> Option<String> {
        None
    }
}

/// The backup's manifest name.
pub const MANIFEST: &str = "manifest.json";
/// The backup format this executor writes: 2, each disk's image holding only
/// what is in use ([`crate::pc_image`]). A restore also reads format 1 (each
/// disk's bytes whole).
pub const BACKUP_FORMAT: i64 = 2;

/// The PC backup's manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupManifest {
    pub backup_format: i64,
    pub kind: String,
    pub disks: Vec<BackupDisk>,
    /// The firmware's boot entries and boot order (`efivars.json`).
    pub variables: BackupFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupDisk {
    pub disk: DiskId,
    pub file: String,
    /// Of the stored stream (format 1: the disk's bytes), as written and as
    /// read back.
    pub sha256: String,
    /// What the image holds (format 2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<crate::pc_image::ImageSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackupFile {
    pub file: String,
    pub sha256: String,
}

/// What the run needs beyond the record.
pub struct PcJob<'a> {
    pub description: &'a Description,
    pub description_sha256: String,
    pub developer: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct PlanView {
    status: String,
    steps: Vec<PlanStep>,
    #[serde(default)]
    choices: Vec<PlanChoice>,
    #[serde(default)]
    partitions: Vec<PlanPart>,
    backup: PlanBackup,
    space: PlanSpace,
}

#[derive(Debug, Clone, Deserialize)]
struct PlanStep {
    id: String,
    mode: String,
    writes: bool,
    tried: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct PlanChoice {
    choice: String,
    value: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PlanPart {
    partition: String,
    class: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    steps: Vec<String>,
    #[serde(default)]
    at: Option<PartAt>,
}

#[derive(Debug, Clone, Deserialize)]
struct PlanBackup {
    choice: String,
    #[serde(default)]
    images: Vec<PlanImage>,
}

#[derive(Debug, Clone, Deserialize)]
struct PlanImage {
    index: u32,
}

#[derive(Debug, Clone, Deserialize)]
struct PlanSpace {
    #[serde(default)]
    disk: Option<PlanDisk>,
}

#[derive(Debug, Clone, Deserialize)]
struct PlanDisk {
    disk: u32,
}

/// What is kept before the first write, so a resume writes the same bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Before {
    disk: DiskId,
    /// The disk's first sectors (its primary table region) as they were.
    head: String,
    head_sha256: String,
    /// The unique GUIDs drawn for this installation: the disk's (used only
    /// when it has no table) and each new partition's, by name.
    guids: BTreeMap<String, String>,
}

const BEFORE: &str = "pc-before.json";
const DISK_GUID_KEY: &str = "disk";

/// Runs (or resumes) the plan of `record` on `machine`.
pub async fn run<M: Machine, E: PcEdge, S: ArtifactSource>(
    job: &PcJob<'_>,
    plan: &str,
    record: &mut Record,
    artifacts: &mut Artifacts<S>,
    machine: &mut M,
    edge: &mut E,
) -> Result<Outcome, Stopped> {
    let mut x = Exec {
        d: job.description,
        job,
        record,
        artifacts,
        m: machine,
        edge,
        pv: None,
        before: None,
        choices: BTreeMap::new(),
    };
    x.run(plan).await
}

struct Exec<'a, M: Machine, E: PcEdge, S: ArtifactSource> {
    d: &'a Description,
    job: &'a PcJob<'a>,
    record: &'a mut Record,
    artifacts: &'a mut Artifacts<S>,
    m: &'a mut M,
    edge: &'a mut E,
    pv: Option<PlanView>,
    before: Option<Before>,
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

/// The description's step a plan step is: `remove-system.1` is the
/// template `remove-system` for system 1.
fn base_id(id: &str) -> &str {
    id.split('.').next().unwrap_or(id)
}

/// Reads a machine's disk as a parser reads.
struct DiskReader<'m, M: Machine> {
    m: &'m mut M,
    disk: u32,
}

impl<M: Machine> ReadAt for DiskReader<'_, M> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        self.m.read(self.disk, offset, buf)
    }
}

/// A byte range a step may write: [start, end).
type Range = (u64, u64);

/// Writes `data` at `offset` of `disk`, refused outside `allowed`.
fn write_in<M: Machine>(m: &mut M, allowed: &[Range], disk: u32, offset: u64, data: &[u8]) -> Result<(), OpError> {
    let end = offset + data.len() as u64;
    if !allowed.iter().any(|(a, b)| offset >= *a && end <= *b) {
        return Err(OpError::before(format!(
            "a write of {} bytes at {offset} is outside what this step may write: refused",
            data.len()
        )));
    }
    m.write(disk, offset, data).map_err(OpError::after)
}

/// The architecture the description's identity names (`cpu.arch`).
fn arch_of(d: &Description) -> String {
    d.identity
        .iter()
        .find(|i| i.key == "cpu.arch")
        .and_then(|i| match &i.equals {
            Some(crate::format1::Scalar::Text(t)) => Some(t.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// Bytes from hex.
fn unhex(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("odd hex".into());
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

impl<M: Machine, E: PcEdge, S: ArtifactSource> Exec<'_, M, E, S> {
    fn step(&self, id: &str) -> Result<&Step, Stopped> {
        let b = base_id(id);
        self.d
            .steps
            .iter()
            .find(|s| s.id == b)
            .ok_or_else(|| stop(format!("the plan names step {id}, which the description doesn't have")))
    }

    fn fail(&self, st: &Step, why: impl Into<String>, may_have_changed: bool, interrupted: bool) -> Stopped {
        fail_in(self.d, st, why, may_have_changed, interrupted)
    }

    fn pv(&self) -> &PlanView {
        self.pv.as_ref().expect("the plan is read first")
    }

    fn save(&mut self, st: Option<&Step>) -> Result<(), Stopped> {
        self.edge.save(self.record).map_err(|e| {
            let why = format!("the install record couldn't be saved ({e}): stopped; resume once it can be");
            match st {
                Some(st) => self.fail(st, why, false, true),
                None => stop(why),
            }
        })
    }

    fn outcome(&mut self, result: Result_, evidence: Evidence, message: Option<String>) -> Done {
        Done {
            result,
            evidence,
            message,
            at: self.edge.now(),
        }
    }

    async fn run(&mut self, plan: &str) -> Result<Outcome, Stopped> {
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
        if pv.space.disk.is_none() {
            return Err(stop("the plan names no target disk: nothing is run"));
        }
        self.choices = pv.choices.iter().map(|c| (c.choice.clone(), c.value.clone())).collect();
        self.pv = Some(pv);
        // Every step's op exists before anything runs.
        for ps in &self.pv().steps {
            let st = self.step(&ps.id)?;
            if ps.mode != st.mode || st.mode != "local" {
                return Err(stop(format!("step {} runs in mode {}, which this executor doesn't drive", ps.id, ps.mode)));
            }
            if !KINDS.contains(&st.kind.as_str()) {
                return Err(stop(format!("step kind {} isn't built for a PC: nothing is run", st.kind)));
            }
        }
        let order = self.order()?;
        self.check_artifacts(&order).await?;
        self.check_backup(&order)?;
        if let Some(b) = self.edge.kept(BEFORE).map_err(|e| stop(format!("what was kept before the first write: {e}")))? {
            self.before = Some(serde_json::from_slice(&b).map_err(|e| stop(format!("{BEFORE}: {e}")))?);
        }
        for id in &order {
            let ps = self.pv().steps.iter().find(|s| s.id == *id).unwrap().clone();
            let st = self.step(id)?.clone();
            if self.record.state(id) == StepState::Done {
                continue;
            }
            if ps.writes && !ps.tried && !self.job.developer {
                return Err(self.fail(&st, "an untried write step: only a developer who asks for it runs it", false, false));
            }
            self.run_step(id, &st).await?;
            if st.safe_stop && self.edge.cancel_requested() {
                return Err(Stopped {
                    step: Some(id.clone()),
                    why: "cancelled after this step, as asked".into(),
                    escape: Some(Box::new(escape_in(self.d, &st.escape.interruption))),
                    may_have_changed: false,
                });
            }
        }
        Ok(Outcome::WaitingForAcceptance {
            mode: "GlassineOS".into(),
            check: "Restart the PC (take the USB stick out first): the firmware starts GlassineOS.".into(),
        })
    }

    fn order(&self) -> Result<Vec<String>, Stopped> {
        let ids: Vec<String> = self.pv().steps.iter().map(|s| s.id.clone()).collect();
        let mut out: Vec<String> = vec![];
        let mut left: VecDeque<String> = ids.iter().cloned().collect();
        let mut stuck = 0;
        let in_plan = |a: &str| ids.iter().any(|i| base_id(i) == a);
        while let Some(id) = left.pop_front() {
            let st = self.step(&id)?;
            if st
                .after
                .iter()
                .all(|a| !in_plan(a) || ids.iter().filter(|i| base_id(i) == a).all(|i| out.contains(i)))
            {
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

    /// The artifact roles a step uses: its parameters', and for the boot
    /// partition the release's parts of it (`pc_boot::boot_roles`).
    fn roles_of(&self, st: &Step) -> Result<Vec<String>, String> {
        step_roles(st, &self.artifacts.manifest)
    }

    async fn check_artifacts(&mut self, order: &[String]) -> Result<(), Stopped> {
        let mut roles = BTreeSet::new();
        for id in order {
            if self.record.state(id) != StepState::Done {
                let st = self.step(id)?;
                roles.extend(self.roles_of(st).map_err(stop)?);
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
        // The boot partition's files can be made (command line, records,
        // splash) before anything is written: the identities' length is fixed.
        for id in order {
            let st = self.step(id)?.clone();
            if self.record.state(id) != StepState::Done && st.kind == "write-boot-files" {
                let any = "00000000-0000-0000-0000-000000000000";
                self.boot_files(&st, any, any).await.map_err(|e| stop(format!("{id}: {}", e.why)))?;
            }
        }
        Ok(())
    }

    /// The backup the edge has is the one the record holds, before anything
    /// is written.
    fn check_backup(&mut self, order: &[String]) -> Result<(), Stopped> {
        let Some(want) = self.record.backup.as_ref().and_then(|b| b.manifest_sha256.clone()) else {
            return Ok(());
        };
        if order.iter().all(|id| self.record.state(id) == StepState::Done) {
            return Ok(());
        }
        let got = self.edge.backup_get(MANIFEST).map_err(|e| stop(format!("the backup: {e}")))?;
        match got.map(|b| sha256_hex(&b)) {
            Some(g) if g == want => Ok(()),
            Some(g) => Err(stop(format!(
                "the backup's manifest is {g}, the record's {want}: not the backup this installation rests on; resume with that backup"
            ))),
            None => Err(stop(format!(
                "the record rests on a backup (manifest {want}) and none is given: resume with that backup"
            ))),
        }
    }

    async fn record_found_done(&mut self, id: &str, st: &Step, why: &str) -> Result<(), Stopped> {
        if self.record.open_step() != Some(id) {
            let at = self.edge.now();
            self.record
                .begin(id, &format!("check whether {id} is done"), at)
                .map_err(|e| self.fail(st, e, false, false))?;
        }
        let mut ev = Evidence::new();
        ev.insert("found-done".into(), why.into());
        let o = self.outcome(Result_::Done, ev, None);
        self.record.finish(id, o).map_err(|e| self.fail(st, e, false, false))?;
        self.save(Some(st))
    }

    async fn run_step(&mut self, id: &str, st: &Step) -> Result<(), Stopped> {
        let open = self.record.open_step() == Some(id);
        if open {
            let o = self.outcome(Result_::Interrupted, Evidence::new(), Some("cut while it ran; run again whole".into()));
            self.record.finish(id, o).map_err(|e| self.fail(st, e, false, true))?;
            self.save(Some(st))?;
        } else if st.kind != "back-up" && self.done(id, st, false)? == Truth::True && self.table_holds(id, st)? {
            return self.record_found_done(id, st, "its done conditions hold").await;
        }
        let mut summary = self.d.texts.steps.get(&st.id).cloned().unwrap_or_else(|| st.kind.clone());
        for role in self.roles_of(st).map_err(|e| self.fail(st, e, false, false))? {
            if let Some(a) = self.record.artifacts.iter().find(|a| a.role == role) {
                summary += &format!(" [{role} sha256 {}]", a.sha256);
            }
        }
        let at = self.edge.now();
        self.record.begin(id, &summary, at).map_err(|e| self.fail(st, e, false, false))?;
        self.save(Some(st))?;
        self.edge.note(&format!("{id}: {summary}"));
        let r = if st.kind == "back-up" {
            self.back_up(id, st).await
        } else {
            self.op(id, st).await
        };
        let ev = match r {
            Ok(ev) => ev,
            Err(e) => {
                let o = self.outcome(Result_::Failed, Evidence::new(), Some(e.why.clone()));
                let _ = self.record.finish(id, o);
                let _ = self.edge.save(self.record);
                return Err(self.fail(st, e.why, e.may_have_changed, false));
            }
        };
        if st.kind != "back-up" {
            match self.done(id, st, true).and_then(|t| Ok((t, self.table_holds(id, st)?))) {
                Ok((Truth::True, true)) => {}
                Ok((Truth::True, false)) => {
                    let why = "the step ran, but the disk's partition table isn't the one it wrote".to_string();
                    let o = self.outcome(Result_::Failed, ev, Some(why.clone()));
                    let _ = self.record.finish(id, o);
                    let _ = self.edge.save(self.record);
                    return Err(self.fail(st, why, true, false));
                }
                r => {
                    let why = match r {
                        Err(s) => s.why,
                        Ok((t, _)) => format!("the step ran, but its done conditions are {t:?}"),
                    };
                    let o = self.outcome(Result_::Failed, ev, Some(why.clone()));
                    let _ = self.record.finish(id, o);
                    let _ = self.edge.save(self.record);
                    return Err(self.fail(st, why, true, false));
                }
            }
        }
        let o = self.outcome(Result_::Done, ev, None);
        self.record.finish(id, o).map_err(|e| self.fail(st, e, true, true))?;
        self.save(Some(st))
    }

    // ---- done ----------------------------------------------------------------

    /// For a table step: both copies of the table on the disk are, byte for
    /// byte, the ones this step writes (a damaged copy the system reads
    /// around isn't done). True for every other step.
    fn table_holds(&mut self, id: &str, st: &Step) -> Result<bool, Stopped> {
        if !matches!(st.kind.as_str(), "remove-partitions" | "create-partitions") || self.before.is_none() {
            return Ok(true);
        }
        let t = self.table_for(id, &st.kind).map_err(|e| self.fail(st, e.why, false, false))?;
        let d = self.target().map_err(|e| self.fail(st, e.why, false, false))?;
        let b = &t.built;
        let mut head = vec![0u8; b.primary.len()];
        let mut tail = vec![0u8; b.backup.len()];
        let r = self
            .m
            .read(d.index, 0, &mut head)
            .and_then(|_| self.m.read(d.index, b.backup_offset(), &mut tail));
        r.map_err(|e| self.fail(st, e, false, false))?;
        Ok(head == b.primary && tail == b.backup)
    }

    /// The step's `done`: its conditions in the installer's own mode (the
    /// record) or on the target disk as it is now. A template step's
    /// `installer.step.<template>` means this one item.
    fn done(&mut self, id: &str, st: &Step, ran: bool) -> Result<Truth, Stopped> {
        let mut all = Truth::True;
        for c in &st.done {
            let t = self.condition(c, id, st, ran)?;
            match t {
                Truth::False => return Ok(Truth::False),
                Truth::Undetermined => all = Truth::Undetermined,
                Truth::True => {}
            }
        }
        Ok(all)
    }

    fn condition(&mut self, c: &Condition, id: &str, st: &Step, ran: bool) -> Result<Truth, Stopped> {
        let mode = c.mode.clone().unwrap_or_else(|| st.mode.clone());
        if mode == INSTALLER_MODE {
            let mut done = self.record.done().unwrap_or_default();
            if ran {
                done.push(id.to_string());
            }
            let me = done.iter().any(|s| s == id);
            // A template's items count for the template only as this one.
            let mut steps_done: Vec<String> = done.into_iter().filter(|s| !s.contains('.')).collect();
            if me && !steps_done.contains(&st.id) {
                steps_done.push(st.id.clone());
            }
            let s = Snapshot {
                mode: INSTALLER_MODE.into(),
                installer: Some(InstallerFacts {
                    backup_verified: self.record.backup.as_ref().is_some_and(|b| b.manifest_sha256.is_some()),
                    steps_done,
                }),
                ..Default::default()
            };
            return Ok(test(c, &s.observe(&c.key, INSTALLER_MODE)));
        }
        if mode != "local" {
            return Ok(Truth::Undetermined);
        }
        let Some(target) = self.pv.as_ref().and_then(|p| p.space.disk.as_ref()).map(|d| d.disk) else {
            return Ok(Truth::Undetermined);
        };
        let disks = self.m.disks().map_err(|e| self.fail(st, e, false, false))?;
        let Some(info) = disks.into_iter().find(|d| d.index == target) else {
            return Ok(Truth::Undetermined);
        };
        let layout = crate::pcfacts::layout(&mut DiskReader { m: &mut *self.m, disk: target }, info.size, info.sector).ok();
        let disk = crate::pcfacts::disk(
            target,
            &crate::pcfacts::DiskInfo {
                kind: info.kind.clone(),
                size: info.size,
                removable: false,
                model: info.model.clone(),
                sector: info.sector,
            },
            layout.as_ref(),
        );
        let s = Snapshot {
            mode: "local".into(),
            storage: Some(Storage {
                names: "user".into(),
                disks: vec![disk],
                installer_disk: None,
            }),
            ..Default::default()
        };
        let o = s.observe(&c.key, "local");
        Ok(if o == Obs::Unknown { Truth::Undetermined } else { test(c, &o) })
    }

    // ---- the target disk -----------------------------------------------------

    fn target_index(&self) -> u32 {
        self.pv().space.disk.as_ref().map(|d| d.disk).unwrap_or(0)
    }

    /// The target disk now: the one the plan names, the same as at the
    /// backup (size, sector, kind, model).
    fn target(&mut self) -> Result<DiskId, OpError> {
        let i = self.target_index();
        let disks = self.m.disks().map_err(OpError::before)?;
        let d = disks
            .into_iter()
            .find(|d| d.index == i)
            .ok_or_else(|| OpError::before(format!("disk {i} isn't there any more")))?;
        if let Some(b) = &self.before
            && b.disk != d
        {
            return Err(OpError::before(format!(
                "disk {i} isn't the disk the backup was made of ({} bytes, {} then; {} bytes, {} now): nothing is written",
                b.disk.size, b.disk.kind, d.size, d.kind
            )));
        }
        Ok(d)
    }

    /// Reads the disk's primary table region as it is now.
    fn head_now(&mut self, d: &DiskId) -> Result<Vec<u8>, OpError> {
        let s = d.sector as usize;
        let mut two = vec![0u8; 2 * s];
        self.m.read(d.index, 0, &mut two).map_err(OpError::before)?;
        let n = pc_table::head_len(&two, d.sector) as usize;
        if n == two.len() {
            return Ok(two);
        }
        let mut all = vec![0u8; n];
        self.m.read(d.index, 0, &mut all).map_err(OpError::before)?;
        Ok(all)
    }

    // ---- back-up ---------------------------------------------------------------

    /// Keeps what makes later writes the same bytes, then backs up every
    /// disk the plan images (or, with the backup off, only the firmware's
    /// boot settings, beside the record), each checked by reading it back.
    async fn back_up(&mut self, id: &str, _st: &Step) -> Result<Evidence, OpError> {
        let d = self.target()?;
        if self.before.is_none() {
            let head = self.head_now(&d)?;
            let mut guids = BTreeMap::new();
            guids.insert(DISK_GUID_KEY.to_string(), self.new_guid());
            for p in self.new_parts_spec()? {
                guids.insert(p.0.clone(), self.new_guid());
            }
            let b = Before {
                disk: d.clone(),
                head_sha256: sha256_hex(&head),
                head: hex(&head),
                guids,
            };
            self.edge
                .keep(BEFORE, &serde_json::to_vec_pretty(&b).unwrap())
                .map_err(|e| OpError::before(format!("keeping the disk's table before the first write: {e}")))?;
            self.before = Some(b);
        }
        let vars = self.boot_variables()?;
        let mut ev = Evidence::new();
        ev.insert("disk".into(), format!("{} ({} bytes)", d.index, d.size));
        ev.insert("table-before-sha256".into(), self.before.as_ref().unwrap().head_sha256.clone());
        if self.pv().backup.choice == "off" || self.choices.get(BACKUP_CHOICE).map(String::as_str) == Some("off") {
            self.edge
                .keep("efivars-before.json", &vars)
                .map_err(|e| OpError::before(format!("keeping the boot settings: {e}")))?;
            ev.insert("backup".into(), "off: no disk imaged; the boot settings kept beside the record".into());
            ev.insert("efivars-sha256".into(), sha256_hex(&vars));
            return Ok(ev);
        }
        let location = self
            .edge
            .backup_location()
            .ok_or_else(|| OpError::before("the plan backs up, and no place for the backup was given"))?;
        let images: Vec<u32> = self.pv().backup.images.iter().map(|i| i.index).collect();
        let disks = self.m.disks().map_err(OpError::before)?;
        let mut out = vec![];
        for i in images {
            let disk = disks
                .iter()
                .find(|x| x.index == i)
                .cloned()
                .ok_or_else(|| OpError::before(format!("disk {i} of the backup isn't there")))?;
            let file = format!("disk{i}.img");
            let bd = crate::pc_image::back_up_disk(&mut *self.m, &mut *self.edge, id, &disk, &file).map_err(OpError::before)?;
            if let Some(im) = &bd.image {
                ev.insert(
                    format!("disk{i}-image"),
                    format!(
                        "{} bytes stored, {} as zeros, {} free space left out",
                        im.data_bytes, im.zero_bytes, im.free_bytes
                    ),
                );
                for p in &im.partitions {
                    self.edge.note(&format!(
                        "  disk {i} partition {}{}: {}",
                        p.partition,
                        p.fs.as_deref().map(|f| format!(" ({f})")).unwrap_or_default(),
                        p.kept
                    ));
                }
            }
            ev.insert(format!("disk{i}-sha256"), bd.sha256.clone());
            out.push(bd);
        }
        let vfile = "efivars.json";
        self.edge.backup_put(vfile, &vars).map_err(OpError::before)?;
        let vback = self.edge.backup_get(vfile).map_err(OpError::before)?;
        if vback.as_deref() != Some(vars.as_slice()) {
            return Err(OpError::before("the firmware's boot settings read back otherwise from the backup"));
        }
        let manifest = BackupManifest {
            backup_format: BACKUP_FORMAT,
            kind: "pc".into(),
            disks: out,
            variables: BackupFile {
                file: vfile.into(),
                sha256: sha256_hex(&vars),
            },
        };
        let mbytes = serde_json::to_vec_pretty(&manifest).unwrap();
        self.edge.backup_put(MANIFEST, &mbytes).map_err(OpError::before)?;
        if self.edge.backup_get(MANIFEST).map_err(OpError::before)?.as_deref() != Some(mbytes.as_slice()) {
            return Err(OpError::before("the backup's manifest read back otherwise"));
        }
        let sha = sha256_hex(&mbytes);
        if let Some(held) = self.record.backup.as_ref().and_then(|b| b.manifest_sha256.clone())
            && held != sha
        {
            return Err(OpError::before(format!(
                "the new backup's manifest is {sha}, the record's {held}: not the backup this installation rests on"
            )));
        }
        self.record.backup = Some(Backup {
            location: location.clone(),
            manifest_sha256: Some(sha.clone()),
        });
        ev.insert("location".into(), location);
        ev.insert("manifest-sha256".into(), sha);
        Ok(ev)
    }

    /// The firmware's boot entries and boot order, as JSON: name → hex.
    fn boot_variables(&mut self) -> Result<Vec<u8>, OpError> {
        let mut out = BTreeMap::new();
        for n in self.m.var_names().map_err(OpError::before)? {
            if (efivar::boot_number(&n).is_some() || n == efivar::global_name("BootOrder"))
                && let Some(b) = self.m.var(&n).map_err(OpError::before)?
            {
                out.insert(n, hex(&b));
            }
        }
        Ok(serde_json::to_vec_pretty(&out).unwrap())
    }

    fn new_guid(&mut self) -> String {
        let mut r = self.edge.random();
        r[6] = (r[6] & 0x0f) | 0x40;
        r[8] = (r[8] & 0x3f) | 0x80;
        let x = hex(&r);
        format!("{}-{}-{}-{}-{}", &x[0..8], &x[8..12], &x[12..16], &x[16..20], &x[20..32])
    }

    // ---- the table --------------------------------------------------------------

    /// The create-partitions step's partitions: name, size, type.
    fn new_parts_spec(&self) -> Result<Vec<(String, NewSize, String)>, OpError> {
        let Some(st) = self.d.steps.iter().find(|s| s.kind == "create-partitions") else {
            return Ok(vec![]);
        };
        let arch = arch_of(self.d);
        let arch = arch.as_str();
        let list = st
            .params
            .get("partitions")
            .and_then(|v| v.as_array())
            .ok_or_else(|| OpError::before("create-partitions names no partitions"))?;
        let mut out = vec![];
        for p in list {
            let name = p
                .get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| OpError::before("a new partition has no name"))?;
            let size_s = p.get("size").and_then(|v| v.as_str()).unwrap_or("");
            let size = if size_s == "rest-of-free-space" {
                let least = p.get("at_least").and_then(|v| v.as_str()).and_then(parse_size).unwrap_or(0);
                NewSize::Rest { at_least: least }
            } else {
                NewSize::Bytes(parse_size(size_s).ok_or_else(|| OpError::before(format!("{name}: size {size_s:?}")))?)
            };
            let ty = match p.get("role").and_then(|v| v.as_str()) {
                Some("esp") => ESP_TYPE,
                Some("system") => root_type(arch).ok_or_else(|| OpError::before(format!("no root partition type is known for {arch:?}")))?,
                _ => LINUX_DATA_TYPE,
            };
            out.push((name.to_string(), size, ty.to_string()));
        }
        Ok(out)
    }

    /// The slots the table steps up to and including `upto` remove (all of
    /// the plan's when `upto` is create-partitions), checked against the
    /// table as it was: each must be the size the plan showed.
    fn removals(&self, upto: Option<&str>) -> Result<Vec<u32>, OpError> {
        let t = self.target_index();
        let mut out = vec![];
        for p in &self.pv().partitions {
            if p.class != "remove" {
                continue;
            }
            let by = |s: &String| base_id(s) == "remove-system" || self.d.steps.iter().any(|x| x.id == base_id(s) && x.kind == "remove-partitions");
            let counts = match upto {
                None => p.steps.iter().any(by),
                Some(u) => p.steps.iter().any(|s| s == u),
            };
            if !counts {
                continue;
            }
            let at =
                p.at.ok_or_else(|| OpError::before(format!("the plan doesn't say where {} is: nothing is removed", p.partition)))?;
            if at.disk != t {
                return Err(OpError::before(format!("{} is on disk {}, not the target disk {t}", p.partition, at.disk)));
            }
            out.push((at.index, p.size));
        }
        // Each against the table as it was before the first write.
        let b = self
            .before
            .as_ref()
            .ok_or_else(|| OpError::before("the disk's table as it was isn't kept: run the backup first"))?;
        let head = unhex(&b.head).map_err(OpError::before)?;
        for (slot, size) in &out {
            let e = &head.get(2 * b.disk.sector as usize + (*slot as usize - 1) * 128..).and_then(|x| x.get(..128));
            let Some(e) = e else {
                return Err(OpError::before(format!("the table as it was has no partition {slot}")));
            };
            let len = (table::le64(e, 40) + 1 - table::le64(e, 32)) * b.disk.sector;
            if Some(len) != *size {
                return Err(OpError::before(format!(
                    "partition {slot} is {len} bytes, the plan showed {size:?}: not the partition the person confirmed; nothing is removed"
                )));
            }
        }
        Ok(out.into_iter().map(|(s, _)| s).collect())
    }

    /// The table a step writes: the one as it was, less what the plan
    /// removes by then, plus GlassineOS's partitions for create-partitions.
    fn table_for(&self, id: &str, kind: &str) -> Result<PcTable, OpError> {
        let b = self
            .before
            .as_ref()
            .ok_or_else(|| OpError::before("the disk's table as it was isn't kept: run the backup first"))?;
        let head = unhex(&b.head).map_err(OpError::before)?;
        let (remove, new) = if kind == "create-partitions" {
            let new = self
                .new_parts_spec()?
                .into_iter()
                .map(|(name, size, type_guid)| {
                    let guid = b.guids.get(&name).cloned().unwrap_or_default();
                    NewPart { name, size, type_guid, guid }
                })
                .collect();
            (self.removals(None)?, new)
        } else {
            (self.removals(Some(id))?, vec![])
        };
        let disk_guid = b.guids.get(DISK_GUID_KEY).cloned().unwrap_or_default();
        pc_table::build(
            &Source {
                head: &head,
                disk_bytes: b.disk.size,
                sector: b.disk.sector,
            },
            &remove,
            &new,
            &disk_guid,
        )
        .map_err(OpError::before)
    }

    /// The tables the plan's table steps write.
    fn built_tables(&self) -> Result<Vec<PcTable>, OpError> {
        let mut out = vec![];
        for ps in &self.pv().steps {
            let st = self.step(&ps.id).map_err(|s| OpError::before(s.why))?;
            if matches!(st.kind.as_str(), "remove-partitions" | "create-partitions") {
                out.push(self.table_for(&ps.id, &st.kind)?);
            }
        }
        Ok(out)
    }

    /// The disk's table is one this installation knows (as it was, or as a
    /// table step wrote it; or, cut inside the primary's write, a disk whose
    /// backup copy is one a table step wrote): checked before every write.
    fn check_head(&mut self, d: &DiskId) -> Result<Vec<u8>, OpError> {
        let now = self.head_now(d)?;
        let b = self.before.as_ref().ok_or_else(|| OpError::before("the disk's table as it was isn't kept"))?;
        if now == unhex(&b.head).map_err(OpError::before)? {
            return Ok(now);
        }
        for t in self.built_tables()? {
            if now == t.built.primary {
                return Ok(now);
            }
        }
        for t in self.built_tables()? {
            let mut tail = vec![0u8; t.built.backup.len()];
            self.m.read(d.index, t.built.backup_offset(), &mut tail).map_err(OpError::before)?;
            if tail == t.built.backup {
                return Ok(now);
            }
        }
        Err(OpError::before(format!(
            "disk {}'s partition table is neither the one backed up nor one this installation wrote: it changed; nothing is written",
            d.index
        )))
    }

    /// The new partitions, as the create-partitions step places them.
    fn placed(&self) -> Result<PcTable, OpError> {
        let st = self
            .d
            .steps
            .iter()
            .find(|s| s.kind == "create-partitions")
            .ok_or_else(|| OpError::before("the description makes no partitions"))?;
        self.table_for(&st.id, "create-partitions")
    }

    /// Reads `len` bytes at `offset` back and compares their sha256.
    fn read_back(&mut self, disk: u32, offset: u64, len: u64, want: &[u8; 32]) -> Result<(), OpError> {
        let mut h = Sha256::new();
        let mut buf = vec![0u8; CHUNK.min(len as usize).max(1)];
        let mut at = 0u64;
        while at < len {
            let n = (len - at).min(buf.len() as u64) as usize;
            self.m.read(disk, offset + at, &mut buf[..n]).map_err(OpError::after)?;
            h.update(&buf[..n]);
            at += n as u64;
        }
        let got: [u8; 32] = h.finalize().into();
        if &got != want {
            return Err(OpError::after(format!(
                "{len} bytes at {offset} read back otherwise than written (sha256 {} for {})",
                hex(&got),
                hex(want)
            )));
        }
        Ok(())
    }

    // ---- the ops ----------------------------------------------------------------

    async fn op(&mut self, id: &str, st: &Step) -> Result<Evidence, OpError> {
        let d = self.target()?;
        match st.kind.as_str() {
            "remove-partitions" | "create-partitions" => self.write_table(id, st, &d),
            "write-base" => self.write_base(id, st, &d).await,
            "write-boot-files" => self.write_boot(id, st, &d).await,
            "add-boot-entry" => self.add_boot_entry(st, &d),
            k => Err(OpError::before(format!("step kind {k} isn't built for a PC"))),
        }
    }

    /// The table: the backup copy first, then the primary, each flushed and
    /// read back. A cut between them leaves the old primary valid (or, cut
    /// in the primary, the new backup copy whole), and a resume writes both
    /// again.
    fn write_table(&mut self, id: &str, st: &Step, d: &DiskId) -> Result<Evidence, OpError> {
        self.check_head(d)?;
        let t = self.table_for(id, &st.kind)?;
        let b = &t.built;
        let s = d.sector;
        let allowed = [(0, b.primary.len() as u64), (b.backup_offset(), d.size / s * s)];
        let (primary, backup, boff) = (b.primary.clone(), b.backup.clone(), b.backup_offset());
        write_in(&mut *self.m, &allowed, d.index, boff, &backup)?;
        self.m.flush(d.index).map_err(OpError::after)?;
        self.read_back(d.index, boff, backup.len() as u64, &Sha256::digest(&backup).into())?;
        write_in(&mut *self.m, &allowed, d.index, 0, &primary)?;
        self.m.flush(d.index).map_err(OpError::after)?;
        self.read_back(d.index, 0, primary.len() as u64, &Sha256::digest(&primary).into())?;
        self.m.table_written(d.index);
        let mut ev = Evidence::new();
        ev.insert("disk".into(), d.index.to_string());
        ev.insert("primary-sha256".into(), hex_sha(&primary));
        ev.insert("backup-sha256".into(), hex_sha(&backup));
        ev.insert("changes".into(), b.describe().join("; "));
        for p in &t.placed {
            ev.insert(format!("partuuid:{}", p.name), p.guid.clone());
            ev.insert(format!("sectors:{}", p.name), format!("{}..{}", p.first, p.last));
        }
        Ok(ev)
    }

    /// A new partition's byte range, from the table this installation
    /// wrote, which the disk must hold now.
    fn extent(&mut self, d: &DiskId, name: &str) -> Result<(u64, u64, pc_table::Placed), OpError> {
        let now = self.check_head(d)?;
        let t = self.placed()?;
        if !t.built.primary.starts_with(&now) || now.len() != t.built.primary.len() {
            return Err(OpError::before("the disk doesn't hold GlassineOS's partitions yet: nothing is written"));
        }
        let p = t
            .placed
            .iter()
            .find(|p| p.name == name)
            .cloned()
            .ok_or_else(|| OpError::before(format!("{name} isn't one of the partitions this installation makes: nothing is written there")))?;
        Ok((p.first * d.sector, (p.last - p.first + 1) * d.sector, p))
    }

    async fn write_base(&mut self, id: &str, st: &Step, d: &DiskId) -> Result<Evidence, OpError> {
        let part = crate::ops::param(st, "partition")?.to_string();
        let role = crate::ops::param(st, "artifact")?.to_string();
        let (start, size, _) = self.extent(d, &part)?;
        let a = self.artifacts.artifact(&role).map_err(|e| OpError::before(e.to_string()))?;
        if a.kind.as_deref() != Some(crate::ops::write_base::POOL_IMAGE) {
            return Err(OpError::before(format!(
                "the release's {role} is of kind {:?}; a PC's system partition takes a {}",
                a.kind.as_deref().unwrap_or("(none)"),
                crate::ops::write_base::POOL_IMAGE
            )));
        }
        let padded = a.size.div_ceil(d.sector) * d.sector;
        if a.size == 0 || padded > size {
            return Err(OpError::before(format!("{role} is {} bytes, {part} holds {size}: not written", a.size)));
        }
        let allowed = [(start, start + size)];
        let mut r = self.artifacts.reader(&role).await.map_err(|e| OpError::before(e.to_string()))?;
        let mut buf = vec![0u8; CHUNK];
        let mut hashes = vec![];
        let mut at = 0u64;
        while at < padded {
            let n = (CHUNK as u64).min(padded - at) as usize;
            let real = (a.size - at.min(a.size)).min(n as u64) as usize;
            buf[..n].fill(0);
            r.read_at(at, &mut buf[..real]).await.map_err(|e| OpError {
                why: e,
                may_have_changed: at > 0,
                goes_on: false,
            })?;
            let data = buf[..n].to_vec();
            write_in(&mut *self.m, &allowed, d.index, start + at, &data)?;
            hashes.push((at, n as u64, <[u8; 32]>::from(Sha256::digest(&data))));
            at += n as u64;
            self.edge.progress(id, at, padded);
        }
        r.finish().map_err(|e| OpError::after(e.to_string()))?;
        self.m.flush(d.index).map_err(OpError::after)?;
        for (off, n, h) in &hashes {
            self.read_back(d.index, start + off, *n, h)?;
        }
        let mut ev = Evidence::new();
        ev.insert("partition".into(), part);
        ev.insert("artifact".into(), role);
        ev.insert("sha256".into(), a.sha256);
        ev.insert("bytes".into(), a.size.to_string());
        ev.insert("written".into(), format!("{padded} bytes at {start}"));
        ev.insert("read-back".into(), format!("{} ranges of {CHUNK} bytes match", hashes.len()));
        Ok(ev)
    }

    /// The boot partition's files for this installation's partitions:
    /// reads the release's parts once (the assembled `initrd`'s sum, the
    /// board's configuration, the splash, which must check).
    async fn boot_files(&mut self, st: &Step, system: &str, userdata: &str) -> Result<BootFiles, OpError> {
        let kernel = crate::ops::param(st, "artifact")?.to_string();
        let initramfs = crate::ops::param(st, "initramfs")?.to_string();
        let loader = crate::ops::param(st, "loader")?.to_string();
        let roles = pc_boot::boot_roles(&self.artifacts.manifest, &kernel, &initramfs, &loader).map_err(OpError::before)?;
        let art = |a: &Artifacts<S>, r: &str| a.artifact(r).map_err(|e| OpError::before(e.to_string()));
        let (k, l) = (art(self.artifacts, &kernel)?, art(self.artifacts, &loader)?);
        let man = self.artifacts.manifest.clone();
        let entry = |r: &str| man.artifacts.get(r).cloned().unwrap_or_default();
        let (krel, irel) = (entry(&kernel), entry(&initramfs));
        // The base command line: board.conf's CMDLINE, and only that.
        let board_role = roles.board.clone().unwrap_or_default();
        let board = read_small(self.artifacts, &board_role, pc_boot::BOARD_MAX).await?;
        let base = pc_boot::board_cmdline(&board).map_err(|e| OpError::before(format!("the release's {board_role}: {e}")))?;
        if let Some(c) = krel.cmdline.as_deref().filter(|c| *c != base) {
            return Err(OpError::before(format!(
                "the release's {kernel} gives the command line {c:?}, its {board_role}'s CMDLINE is {base:?}: a release names one command line, not written"
            )));
        }
        // The kernel set's initrd: as the release hands it over, or assembled.
        let (initrd, content) = if roles.assembled {
            let i = art(self.artifacts, &initramfs)?;
            ((i.sha256, i.size), Content::Artifact(initramfs.clone()))
        } else {
            let mut sizes = BTreeMap::new();
            for r in roles.all() {
                sizes.insert(r.clone(), art(self.artifacts, &r)?.size);
            }
            let parts = pc_boot::initrd_parts(&roles, &board, &|r| sizes.get(r).copied().unwrap_or(0));
            let mut h = Sha256::new();
            let mut n = 0u64;
            for p in &parts {
                match p {
                    Part::Bytes(b) => {
                        h.update(b);
                        n += b.len() as u64;
                    }
                    Part::Role(r) => n += hash_role(self.artifacts, r, &mut h).await?,
                }
            }
            ((hex(&h.finalize()), n), Content::Parts(parts))
        };
        let set = KernelSet::new(
            (&k.sha256, k.size),
            Some((&initrd.0, initrd.1)),
            &base,
            system,
            userdata,
            krel.release.as_deref().unwrap_or(""),
            // the initramfs's identity is known only when the installer assembles
            if roles.assembled { "" } else { irel.identity.as_deref().unwrap_or("") },
        )
        .map_err(OpError::before)?;
        let recovery = match &roles.recovery {
            None => None,
            Some(r) => {
                let ra = art(self.artifacts, r)?;
                let rrel = entry(r);
                let ri = match &roles.recovery_initrd {
                    Some(x) => Some(art(self.artifacts, x)?),
                    None => None,
                };
                let rset = KernelSet::new(
                    (&ra.sha256, ra.size),
                    ri.as_ref().map(|i| (i.sha256.as_str(), i.size)),
                    rrel.cmdline.as_deref().unwrap_or(""),
                    system,
                    userdata,
                    rrel.release.as_deref().unwrap_or(""),
                    "",
                )
                .map_err(|e| OpError::before(format!("the recovery image: {e}")))?;
                Some((rset, r.clone(), roles.recovery_initrd.clone()))
            }
        };
        let splash = match &roles.splash {
            None => None,
            Some(r) => {
                let b = read_small(self.artifacts, r, pc_boot::SPLASH_MAX).await?;
                pc_boot::check_splash(&b).map_err(|e| OpError::before(format!("the release's {r}: {e}: not written")))?;
                Some((r.clone(), b.len() as u64))
            }
        };
        if set.id == recovery.as_ref().map(|r| r.0.id.clone()).unwrap_or_default() {
            return Err(OpError::before("the recovery image and the kernel set are the same set"));
        }
        pc_boot::files(
            &arch_of(self.d),
            (&loader, l.size),
            &set,
            &kernel,
            content,
            recovery.as_ref(),
            splash.as_ref().map(|(r, n)| (r.as_str(), *n)),
        )
        .map_err(OpError::before)
    }

    /// Writes the boot partition: no file system there while it is written
    /// (the boot sector zeroed first), the files, the tables and slot a of
    /// the loader record, then the boot sector, then slot b (zeroed in the
    /// first write, so a cut while it is written leaves slot a the one valid
    /// slot, never a stale record there).
    async fn write_boot(&mut self, _id: &str, st: &Step, d: &DiskId) -> Result<Evidence, OpError> {
        let part = crate::ops::param(st, "partition")?.to_string();
        let (start, size, placed) = self.extent(d, &part)?;
        let t = self.placed()?;
        let uuid = |n: &str| t.placed.iter().find(|p| p.name == n).map(|p| p.guid.clone());
        let system = uuid("glassine-system").ok_or_else(|| OpError::before("the plan makes no glassine-system"))?;
        let data_name = self
            .d
            .data_partition
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_else(|| "glassine-userdata".into());
        let userdata = uuid(&data_name).ok_or_else(|| OpError::before(format!("the plan makes no {data_name}")))?;
        let bf = self.boot_files(st, &system, &userdata).await?;
        let specs: Vec<fat::FileSpec> = bf.files.iter().map(|(f, _)| f.clone()).collect();
        let serial = u32::from_le_bytes(crate::gpt_build::guid_bytes(&placed.guid).map_err(OpError::before)?[0..4].try_into().unwrap());
        let lay = fat::layout(size, d.sector, placed.first, serial, pc_boot::VOLUME_LABEL, &specs).map_err(OpError::before)?;
        let allowed = [(start, start + size)];
        let s = d.sector as usize;
        let slot_b = lay.files.len() - 1;
        let (_, b_off, b_len) = lay.files[slot_b].clone();
        // 1. No file system there while it is written, and no old slot b.
        let zero = vec![0u8; s];
        for at in [0u64, 6] {
            write_in(&mut *self.m, &allowed, d.index, start + at * d.sector, &zero)?;
        }
        write_in(&mut *self.m, &allowed, d.index, start + b_off, &vec![0u8; b_len as usize])?;
        self.m.flush(d.index).map_err(OpError::after)?;
        // 2. The files but slot b, the FATs and the directories.
        let mut checks: Vec<(u64, u64, [u8; 32])> = vec![];
        let mut later = None;
        for (k, ((path, off, len), (_, content))) in lay.files.iter().zip(&bf.files).enumerate() {
            let at = start + off;
            match content {
                Content::Bytes(b) if k == slot_b => later = Some((at, b.clone())),
                Content::Bytes(b) => {
                    write_in(&mut *self.m, &allowed, d.index, at, b)?;
                    checks.push((at, *len, Sha256::digest(b).into()));
                }
                Content::Artifact(role) => {
                    let mut h = Sha256::new();
                    stream_role(self.artifacts, &mut *self.m, &allowed, d.index, at, role, &mut h).await?;
                    checks.push((at, *len, h.finalize().into()));
                }
                Content::Parts(parts) => {
                    let mut h = Sha256::new();
                    let mut o = at;
                    for p in parts {
                        match p {
                            Part::Bytes(b) => {
                                write_in(&mut *self.m, &allowed, d.index, o, b)?;
                                h.update(b);
                                o += b.len() as u64;
                            }
                            Part::Role(r) => o += stream_role(self.artifacts, &mut *self.m, &allowed, d.index, o, r, &mut h).await?,
                        }
                    }
                    let got: [u8; 32] = h.finalize().into();
                    if o - at != *len || bf.set.initrd.as_ref().map(|i| i.0.as_str()) != Some(hex(&got).as_str()) {
                        return Err(OpError::after(format!("{path}: the parts written aren't the ones the kernel set names")));
                    }
                    checks.push((at, *len, got));
                }
            }
        }
        for (off, b) in &lay.tables {
            write_in(&mut *self.m, &allowed, d.index, start + off, b)?;
            checks.push((start + off, b.len() as u64, Sha256::digest(b).into()));
        }
        self.m.flush(d.index).map_err(OpError::after)?;
        // 3. The boot sector: from here on the volume is whole, with slot a.
        let (roff, rb) = lay.reserved.clone();
        write_in(&mut *self.m, &allowed, d.index, start + roff, &rb)?;
        self.m.flush(d.index).map_err(OpError::after)?;
        checks.push((start + roff, rb.len() as u64, Sha256::digest(&rb).into()));
        // 4. Slot b, the same record.
        let (bat, bb) = later.ok_or_else(|| OpError::before("the loader record's slot b is missing"))?;
        write_in(&mut *self.m, &allowed, d.index, bat, &bb)?;
        self.m.flush(d.index).map_err(OpError::after)?;
        checks.push((bat, bb.len() as u64, Sha256::digest(&bb).into()));
        for (off, len, h) in &checks {
            self.read_back(d.index, *off, *len, h)?;
        }
        let mut ev = Evidence::new();
        ev.insert("partition".into(), part);
        ev.insert("kernel-set".into(), bf.set.id.clone());
        ev.insert("cmdline".into(), bf.set.cmdline.clone());
        if let Some(r) = &bf.recovery {
            ev.insert("recovery".into(), r.id.clone());
        }
        ev.insert(
            "loader".into(),
            format!("{} and {}", pc_boot::firmware_path(&specs[0].path), pc_boot::firmware_path(&specs[1].path)),
        );
        ev.insert("loader-record-sha256".into(), hex_sha(bf.loader_record.as_bytes()));
        ev.insert("read-back".into(), format!("{} regions match", checks.len()));
        ev.insert("free-bytes".into(), lay.free_bytes.to_string());
        Ok(ev)
    }

    /// Our entry: the one whose partition is the boot partition (by its
    /// unique GUID), else a new one at the lowest free number; then the
    /// boot order with ours first and the others in their order.
    fn add_boot_entry(&mut self, st: &Step, d: &DiskId) -> Result<Evidence, OpError> {
        let part = crate::ops::param(st, "partition")?.to_string();
        let (_, _, p) = self.extent(d, &part)?;
        let hd = efivar::HardDrive {
            number: p.slot,
            start: p.first,
            blocks: p.last - p.first + 1,
            guid: crate::gpt_build::guid_bytes(&p.guid).map_err(OpError::before)?,
        };
        let want = efivar::load_option_file(pc_boot::ENTRY_LABEL, &hd, &pc_boot::firmware_path(pc_boot::LOADER));
        let names = self.m.var_names().map_err(OpError::before)?;
        let mut numbers: Vec<u16> = names.iter().filter_map(|n| efivar::boot_number(n)).collect();
        numbers.sort();
        let mut ours = None;
        for n in &numbers {
            let Some(b) = self.m.var(&efivar::boot_name(*n)).map_err(OpError::before)? else {
                continue;
            };
            if efivar::load_option(&b)
                .and_then(|o| o.partition_guid)
                .is_some_and(|g| g.eq_ignore_ascii_case(&p.guid))
            {
                ours = Some((*n, b));
                break;
            }
        }
        let mut ev = Evidence::new();
        let number = match ours {
            Some((n, b)) if b == want => {
                ev.insert("entry".into(), format!("Boot{n:04X}, already there"));
                n
            }
            Some((n, _)) => {
                self.set_checked(&efivar::boot_name(n), &want)?;
                ev.insert("entry".into(), format!("Boot{n:04X}, rewritten"));
                n
            }
            None => {
                let n = (0u16..=0xffff)
                    .find(|n| !numbers.contains(n))
                    .ok_or_else(|| OpError::before("the firmware has no free boot entry number"))?;
                self.set_checked(&efivar::boot_name(n), &want)?;
                ev.insert("entry".into(), format!("Boot{n:04X}, new"));
                n
            }
        };
        let order_name = efivar::global_name("BootOrder");
        let order = self
            .m
            .var(&order_name)
            .map_err(OpError::after)?
            .map(|b| efivar::number_list(&b))
            .unwrap_or_default();
        let mut new_order = vec![number];
        new_order.extend(order.iter().copied().filter(|n| *n != number));
        if order != new_order {
            self.set_checked(&order_name, &efivar::numbers_file(&new_order))?;
        }
        ev.insert("boot-order".into(), new_order.iter().map(|n| format!("{n:04X}")).collect::<Vec<_>>().join(","));
        ev.insert("loader".into(), pc_boot::firmware_path(pc_boot::LOADER));
        Ok(ev)
    }

    fn set_checked(&mut self, name: &str, file: &[u8]) -> Result<(), OpError> {
        self.m.set_var(name, file).map_err(OpError::after)?;
        let back = self.m.var(name).map_err(OpError::after)?;
        if back.as_deref() != Some(file) {
            return Err(OpError::after(format!("the firmware's {name} reads back otherwise than written")));
        }
        Ok(())
    }
}

/// The step kinds this executor carries out.
pub const KINDS: &[&str] = &[
    "back-up",
    "remove-partitions",
    "create-partitions",
    "write-base",
    "write-boot-files",
    "add-boot-entry",
];

/// The artifact roles a step uses: its parameters', and for a
/// `write-boot-files` step the release's parts of the boot partition.
pub fn step_roles(st: &Step, release: &crate::release::Release) -> Result<Vec<String>, String> {
    let p = |k: &str| st.params.get(k).and_then(|v| v.as_str()).map(String::from);
    if st.kind == "write-boot-files"
        && let (Some(k), Some(i), Some(l)) = (p("artifact"), p("initramfs"), p("loader"))
    {
        return Ok(pc_boot::boot_roles(release, &k, &i, &l)?.all());
    }
    Ok(["artifact", "initramfs", "loader"].iter().filter_map(|k| p(k)).collect())
}

/// Reads a role's file whole (at most `max` bytes), checked.
async fn read_small<S: ArtifactSource>(arts: &mut Artifacts<S>, role: &str, max: u64) -> Result<Vec<u8>, OpError> {
    let a = arts.artifact(role).map_err(|e| OpError::before(e.to_string()))?;
    if a.size > max {
        return Err(OpError::before(format!("the release's {role} is {} bytes, at most {max} are read", a.size)));
    }
    let mut r = arts.reader(role).await.map_err(|e| OpError::before(e.to_string()))?;
    let mut b = vec![0u8; a.size as usize];
    if !b.is_empty() {
        r.read_at(0, &mut b).await.map_err(OpError::before)?;
    }
    r.finish().map_err(|e| OpError::before(e.to_string()))?;
    Ok(b)
}

/// Hashes a role's file into `h`, checked; its size.
async fn hash_role<S: ArtifactSource>(arts: &mut Artifacts<S>, role: &str, h: &mut Sha256) -> Result<u64, OpError> {
    let mut r = arts.reader(role).await.map_err(|e| OpError::before(e.to_string()))?;
    let len = r.artifact.size;
    let mut buf = vec![0u8; CHUNK.min(len as usize)];
    let mut at = 0u64;
    while at < len {
        let n = (CHUNK as u64).min(len - at) as usize;
        r.read_at(at, &mut buf[..n]).await.map_err(OpError::before)?;
        h.update(&buf[..n]);
        at += n as u64;
    }
    r.finish().map_err(|e| OpError::before(e.to_string()))?;
    Ok(len)
}

/// Streams a role's file to `at` of the disk, checked, hashing it into `h`;
/// its size.
async fn stream_role<S: ArtifactSource, M: Machine>(
    arts: &mut Artifacts<S>,
    m: &mut M,
    allowed: &[Range],
    disk: u32,
    at: u64,
    role: &str,
    h: &mut Sha256,
) -> Result<u64, OpError> {
    let mut r = arts.reader(role).await.map_err(|e| OpError::before(e.to_string()))?;
    let len = r.artifact.size;
    let mut buf = vec![0u8; CHUNK.min(len as usize)];
    let mut o = 0u64;
    while o < len {
        let n = (CHUNK as u64).min(len - o) as usize;
        r.read_at(o, &mut buf[..n]).await.map_err(OpError::after)?;
        write_in(m, allowed, disk, at + o, &buf[..n])?;
        h.update(&buf[..n]);
        o += n as u64;
    }
    r.finish().map_err(|e| OpError::after(format!("{role}: {e}")))?;
    Ok(len)
}

/// The escape of a step, for callers that show it.
pub fn escape_of(d: &Description, st: &Step, interrupted: bool) -> Escape {
    escape_in(d, if interrupted { &st.escape.interruption } else { &st.escape.failure })
}
