// SPDX-License-Identifier: GPL-2.0-or-later
//! What an installation may write through one of a description's fastboot
//! modes (002 FR-011, FR-014, FR-022), derived from the description alone:
//! the framing its writes need, the partitions its rules never let anything
//! write, the `oem` verbs it allows, the operations it marks never-use, and
//! the partitions the job's steps in that mode write. [`ModeRules::check`]
//! allows a partition only when a step in the mode writes it and nothing
//! refuses it; every other name is refused before anything is sent.
//!
//! Two refusals hold whatever the description says:
//! - [`crate::fastboot::NOT_PARTITIONS`]: names the boot loader takes for the
//!   partition table or a verified-boot key ([`crate::fastboot::WriteRules`]).
//! - [`SYSTEM_PREFIX`]: abl's `flash` of a partition whose name starts with
//!   `system` switches verified boot to enforcing first, and that writes
//!   `devinfo` (`CmdFlash` → `EnableEnforcingMode`). `devinfo` holds the
//!   unlock state; GlassineOS never writes it.

use crate::fastboot::{FbError, Framing, WriteRules};
use crate::format1::{Description, Mode};
use crate::inspect;
use crate::vocab;

/// The prefix of the partition names never written over fastboot, and why.
pub const SYSTEM_PREFIX: (&str, &str) = (
    "system",
    "the boot loader switches verified boot to enforcing before it writes a partition whose name starts with \"system\", and that writes devinfo, which GlassineOS never writes",
);

/// The `oem` verb of the `oem-reboot-recovery` departure.
pub const OEM_REBOOT_RECOVERY_VERB: &str = "reboot-recovery";

/// An operation a mode's description marks never-use, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeverUse {
    pub operation: String,
    pub reason: String,
}

/// What a fastboot mode may write, from its description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeRules {
    pub description: String,
    pub mode: String,
    /// The framing, never-write names and `oem` verbs, for the session's own checks.
    pub rules: WriteRules,
    /// The partitions the job's steps in this mode write: the only names allowed.
    pub writes: Vec<String>,
    /// The operations the mode's description marks never-use.
    pub never_use: Vec<NeverUse>,
}

impl ModeRules {
    /// Refuses `part` unless a step in this mode writes it and nothing
    /// refuses it: never a name that starts with [`SYSTEM_PREFIX`] (any
    /// case), never one [`WriteRules::check_partition`] refuses, never a write
    /// when the mode marks `flash` never-use.
    pub fn check(&self, part: &str) -> Result<(), FbError> {
        let lower = part.to_ascii_lowercase();
        if lower.starts_with(SYSTEM_PREFIX.0) {
            return Err(FbError::RefusedWrite(format!("flash:{part}: {}", SYSTEM_PREFIX.1)));
        }
        self.check_operation("flash")?;
        self.rules.check_partition(part)?;
        if !self.writes.iter().any(|w| w.eq_ignore_ascii_case(part)) {
            return Err(FbError::RefusedWrite(format!(
                "no step of the installation writes {part} in {} mode {}",
                self.description, self.mode
            )));
        }
        Ok(())
    }

    /// Refuses an operation the mode's description marks never-use.
    pub fn check_operation(&self, operation: &str) -> Result<(), FbError> {
        match self.never_use.iter().find(|n| n.operation == operation) {
            Some(n) => Err(FbError::RefusedWrite(format!(
                "{} mode {} says never to use {operation}: {}",
                self.description, self.mode, n.reason
            ))),
            None => Ok(()),
        }
    }

    /// Refuses an `oem` verb the mode doesn't allow.
    pub fn check_oem(&self, verb: &str) -> Result<(), FbError> {
        if self.rules.oem.iter().any(|v| v == verb) {
            return Ok(());
        }
        Err(FbError::RefusedWrite(format!("{} mode {} allows no oem {verb:?}", self.description, self.mode)))
    }
}

/// The write rules of fastboot mode `mode` of `d`, for the steps of job `job`.
pub fn mode_rules(d: &Description, mode: &str, job: &str) -> Result<ModeRules, String> {
    let m: &Mode = d.mode(mode).ok_or_else(|| format!("{} has no mode {mode:?}", d.id))?;
    if m.kind != "fastboot" {
        return Err(format!("{} mode {mode} is a {} mode, not fastboot", d.id, m.kind));
    }
    let at = format!("{} mode {mode}", d.id);
    let framing = framing(d, m).map_err(|e| format!("{at}: {e}"))?;
    let mut never_write: Vec<String> = Vec::new();
    for r in d.partitions.iter().filter(|r| matches!(r.class.as_str(), "never-write" | "read" | "back-up")) {
        for n in r.select.name.iter().chain(r.select.names.iter().flatten()) {
            if !never_write.contains(n) {
                never_write.push(n.clone());
            }
        }
    }
    let oem = if m.departures.iter().any(|x| x.kind == vocab::OEM_REBOOT_RECOVERY) {
        vec![OEM_REBOOT_RECOVERY_VERB.to_string()]
    } else {
        vec![]
    };
    let never_use = m
        .departures
        .iter()
        .filter(|x| x.kind == "never-use")
        .filter_map(|x| {
            Some(NeverUse {
                operation: x.params.get("operation")?.as_str()?.to_string(),
                reason: x.params.get("reason").and_then(|v| v.as_str()).unwrap_or("no reason given").to_string(),
            })
        })
        .collect();
    let mut writes: Vec<String> = Vec::new();
    if m.can.iter().any(|c| c == "write") {
        for s in d.steps.iter().filter(|s| s.job == job && s.mode == mode) {
            for w in s.writes.iter().filter(|w| !w.starts_with("role:")) {
                if !writes.contains(w) {
                    writes.push(w.clone());
                }
            }
        }
    }
    Ok(ModeRules {
        description: d.id.clone(),
        mode: mode.to_string(),
        rules: WriteRules { framing, never_write, oem },
        writes,
        never_use,
    })
}

/// The framing the mode's `equal-length-transfers` departure tells (the
/// table inspecting reads), else plain.
fn framing(d: &Description, m: &Mode) -> Result<Framing, String> {
    if !m.departures.iter().any(|x| x.kind == "equal-length-transfers") {
        return Ok(Framing::Plain);
    }
    let table = inspect::framing_table(&[d]).map_err(|e| e.to_string())?;
    table
        .iter()
        .find(|r| r.description == d.id && r.mode == m.id)
        .map(|r| r.framing)
        .ok_or_else(|| "its equal-length-transfers departure isn't told by the product, so its framing can't be known".into())
}
