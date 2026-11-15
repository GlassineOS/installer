// SPDX-License-Identifier: GPL-2.0-or-later
//! The install record: what was confirmed and what was done. It holds the
//! plan exactly as the person confirmed it (and its hash), the confirmation,
//! the description's hash, the installer's version, the artifacts' hashes,
//! where the backup is, and a journal of steps: the intent is recorded before
//! a step writes anything, the outcome with its evidence after. A record
//! whose last entry has an intent and no outcome was interrupted in that step.
//!
//! Saving is atomic through the edge ([`RecordStore`]): a whole new copy
//! under a temporary name, then a rename over the old one, so a cut leaves
//! the previous record readable. The core has no clock: times come from the
//! edge as text.

use crate::artifacts::Artifact;
use crate::lock::sha256_hex;
use crate::plan::PLAN_FORMAT;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The record format this installer writes and reads (and every earlier one).
pub const RECORD_FORMAT: i64 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub record_format: i64,
    pub installer_version: String,
    pub description: DescriptionRef,
    /// The plan's JSON, byte for byte as it was shown and confirmed.
    pub plan: String,
    pub plan_sha256: String,
    pub confirmation: Confirmation,
    /// By role.
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    /// None when the person chose to go without a backup.
    #[serde(default)]
    pub backup: Option<Backup>,
    #[serde(default)]
    pub journal: Vec<Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DescriptionRef {
    pub id: String,
    /// The SHA-256 of the description file as read (the lock file's hash).
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Confirmation {
    /// What the person confirmed with, as shown (the words typed, the button).
    pub text: String,
    #[serde(default)]
    pub at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Backup {
    /// Where the backup is, as the edge names it (a folder, a drive).
    pub location: String,
    /// The SHA-256 of the backup's manifest, once it is verified.
    #[serde(default)]
    pub manifest_sha256: Option<String>,
}

/// One attempt at one step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub step: String,
    pub intent: Intent,
    #[serde(default)]
    pub outcome: Option<Outcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    /// What the step is about to do, for the person.
    pub summary: String,
    #[serde(default)]
    pub at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub result: Result_,
    /// What shows it: read-back hashes, the device's answers.
    #[serde(default)]
    pub evidence: BTreeMap<String, String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Result_ {
    Done,
    Failed,
    /// Closed on resume: the attempt was cut and its effect is unknown or
    /// was checked and found incomplete.
    Interrupted,
}

/// Where a step stands, from the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    NotStarted,
    /// An intent with no outcome: the step was cut while it ran.
    Open,
    Done,
    Failed,
    Interrupted,
}

/// What a resumed run does next, from the plan's step order and the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resume {
    /// This step was cut: check its effect and close it before anything else.
    Interrupted(String),
    /// This step failed last: its escape, or a retry the person chose.
    Failed(String),
    /// The next step not done yet.
    Next(String),
    Complete,
}

impl Record {
    /// A new record, before any step. `plan` is the plan's JSON exactly as
    /// confirmed; `description` the description file's bytes.
    pub fn new(
        installer_version: &str,
        description_id: &str,
        description: &[u8],
        plan: &str,
        confirmation: Confirmation,
        artifacts: Vec<Artifact>,
        backup: Option<Backup>,
    ) -> Result<Record, String> {
        let r = Record {
            record_format: RECORD_FORMAT,
            installer_version: installer_version.into(),
            description: DescriptionRef {
                id: description_id.into(),
                sha256: sha256_hex(description),
            },
            plan: plan.into(),
            plan_sha256: sha256_hex(plan.as_bytes()),
            confirmation,
            artifacts,
            backup,
            journal: vec![],
        };
        r.plan_steps()?;
        Ok(r)
    }

    /// The plan's step ids, in order.
    pub fn plan_steps(&self) -> Result<Vec<String>, String> {
        let v: serde_json::Value = serde_json::from_str(&self.plan).map_err(|e| format!("the plan is not JSON: {e}"))?;
        match v.get("plan_format").and_then(|f| f.as_i64()) {
            Some(f) if (1..=PLAN_FORMAT).contains(&f) => {}
            _ => return Err("the plan has no plan format this installer reads".into()),
        }
        let steps = v.get("steps").and_then(|s| s.as_array()).ok_or("the plan has no steps")?;
        steps
            .iter()
            .map(|s| {
                s.get("id")
                    .and_then(|i| i.as_str())
                    .map(String::from)
                    .ok_or_else(|| "a plan step has no id".into())
            })
            .collect()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a record serializes") + "\n"
    }

    /// Reads a record and checks it: a format this installer reads, the plan
    /// matching its hash, a journal where only the last entry may be open.
    pub fn parse(json: &str) -> Result<Record, String> {
        let v: serde_json::Value = serde_json::from_str(json).map_err(|e| format!("not an install record: {e}"))?;
        let f = v
            .get("record_format")
            .and_then(|f| f.as_i64())
            .ok_or("not an install record: no record_format")?;
        if !(1..=RECORD_FORMAT).contains(&f) {
            return Err(format!("record format {f} is not one this installer reads (it reads up to {RECORD_FORMAT})"));
        }
        let r: Record = serde_json::from_value(v).map_err(|e| format!("not an install record: {e}"))?;
        if sha256_hex(r.plan.as_bytes()) != r.plan_sha256 {
            return Err("the record's plan doesn't match its hash".into());
        }
        let steps = r.plan_steps()?;
        for (i, e) in r.journal.iter().enumerate() {
            if !steps.contains(&e.step) {
                return Err(format!("the journal names {}, which the plan doesn't have", e.step));
            }
            if e.outcome.is_none() && i + 1 != r.journal.len() {
                return Err(format!("the journal has an open entry for {} before others", e.step));
            }
        }
        Ok(r)
    }

    /// Where `step` stands: its latest entry.
    pub fn state(&self, step: &str) -> StepState {
        match self.journal.iter().rev().find(|e| e.step == step) {
            None => StepState::NotStarted,
            Some(Entry { outcome: None, .. }) => StepState::Open,
            Some(Entry { outcome: Some(o), .. }) => match o.result {
                Result_::Done => StepState::Done,
                Result_::Failed => StepState::Failed,
                Result_::Interrupted => StepState::Interrupted,
            },
        }
    }

    /// The open entry's step, if a step was cut while it ran.
    pub fn open_step(&self) -> Option<&str> {
        self.journal.last().filter(|e| e.outcome.is_none()).map(|e| e.step.as_str())
    }

    /// The steps done, in plan order.
    pub fn done(&self) -> Result<Vec<String>, String> {
        Ok(self.plan_steps()?.into_iter().filter(|s| self.state(s) == StepState::Done).collect())
    }

    /// What a resumed run does next.
    pub fn resume(&self) -> Result<Resume, String> {
        if let Some(s) = self.open_step() {
            return Ok(Resume::Interrupted(s.into()));
        }
        if let Some(e) = self.journal.last()
            && let Some(o) = &e.outcome
            && o.result == Result_::Failed
        {
            return Ok(Resume::Failed(e.step.clone()));
        }
        Ok(match self.plan_steps()?.into_iter().find(|s| self.state(s) != StepState::Done) {
            Some(s) => Resume::Next(s),
            None => Resume::Complete,
        })
    }

    /// Records the intent of `step` before it runs. Refused while another
    /// entry is open, for a step already done, or one the plan doesn't have.
    pub fn begin(&mut self, step: &str, summary: &str, at: Option<String>) -> Result<(), String> {
        if let Some(s) = self.open_step() {
            return Err(format!("{s} is still open: close it before {step} starts"));
        }
        if !self.plan_steps()?.iter().any(|s| s == step) {
            return Err(format!("the plan has no step {step}"));
        }
        if self.state(step) == StepState::Done {
            return Err(format!("{step} is already done"));
        }
        self.journal.push(Entry {
            step: step.into(),
            intent: Intent { summary: summary.into(), at },
            outcome: None,
        });
        Ok(())
    }

    /// Records the outcome of the open step, which must be `step`.
    pub fn finish(&mut self, step: &str, outcome: Outcome) -> Result<(), String> {
        match self.journal.last_mut() {
            Some(e) if e.outcome.is_none() && e.step == step => {
                e.outcome = Some(outcome);
                Ok(())
            }
            _ => Err(format!("{step} has no open entry")),
        }
    }
}

/// Where the record is kept: the edge's file (or the browser's storage).
pub trait RecordStore {
    /// Writes `bytes` whole under the temporary name, durably (synced),
    /// replacing any earlier temporary copy. The record itself is untouched.
    fn write_temp(&mut self, bytes: &[u8]) -> Result<(), String>;
    /// Renames the temporary copy over the record, durably.
    fn commit(&mut self) -> Result<(), String>;
    /// The record's bytes, or None when there is none yet.
    fn read(&mut self) -> Result<Option<Vec<u8>>, String>;
}

/// Saves the record: the temporary copy, read back and checked, then the rename.
pub fn save(store: &mut dyn RecordStore, record: &Record) -> Result<(), String> {
    let text = record.to_json();
    store.write_temp(text.as_bytes())?;
    store.commit()?;
    match store.read()? {
        Some(b) if b == text.as_bytes() => Ok(()),
        _ => Err("the saved record doesn't read back as written".into()),
    }
}

/// The saved record, checked; None when there is none.
pub fn load(store: &mut dyn RecordStore) -> Result<Option<Record>, String> {
    match store.read()? {
        None => Ok(None),
        Some(b) => {
            let t = String::from_utf8(b).map_err(|_| "the record is not UTF-8".to_string())?;
            Record::parse(&t).map(Some)
        }
    }
}
