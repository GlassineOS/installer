// SPDX-License-Identifier: GPL-2.0-or-later
//! The shareable check report (001 Scenario 2). It is built from an allow-list
//! of typed fields: nothing reaches it unless a field here takes it. Never in
//! it: serial numbers, disk and partition GUIDs, file-system UUIDs and labels,
//! a person's partition labels, firmware boot-entry labels and paths, SMBIOS
//! serial and UUID, MAC addresses, host names. Fastboot variables and Android
//! properties outside the allow-lists appear by name only.

use crate::facts::Snapshot;
use crate::format1::Description;
use crate::plan::Plan;
use crate::recognize::{Outcome, Recognition, StateResult, Verdict};
use crate::vocab;
use serde::Serialize;
use std::collections::BTreeMap;

pub const REPORT_FORMAT: i64 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub report_format: i64,
    pub mode: String,
    pub runs_on: Option<RunsOn>,
    pub machine: Machine,
    pub recognition: RecognitionReport,
    pub supported: bool,
    pub reasons: Vec<String>,
    pub capabilities: Vec<Capability>,
    pub plan: Option<PlanSummary>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunsOn {
    pub platform: String,
    pub connections: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Machine {
    pub usb: Option<Usb>,
    pub fastboot: Option<Vars>,
    pub android: Option<Vars>,
    pub disks: Vec<Disk>,
    pub firmware: Option<Firmware>,
    pub cpu: Option<Cpu>,
    pub board: Option<Board>,
    pub systems: Vec<System>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Usb {
    pub vendor_id: String,
    pub product_id: String,
    pub interfaces: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Vars {
    pub values: BTreeMap<String, Option<String>>,
    /// Asked and answered, shown by name only.
    pub other_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Disk {
    pub kind: String,
    pub size: u64,
    pub removable: bool,
    pub table: Option<String>,
    pub partitions: Vec<Partition>,
    pub free: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Partition {
    /// Only where names are the maker's, never a person's labels.
    pub name: Option<String>,
    pub size: u64,
    pub role: Option<String>,
    pub filesystem: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Firmware {
    pub kind: String,
    pub secure_boot: Option<String>,
    pub boot_entries: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Cpu {
    pub arch: String,
    pub vendor: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Board {
    pub vendor: Option<String>,
    pub product: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct System {
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RecognitionReport {
    pub result: String,
    pub device: Option<String>,
    pub name: Option<String>,
    pub state: Option<String>,
    pub candidates: Vec<String>,
    pub needs_modes: Vec<String>,
    /// For a conflict: the signals that hold and those that contradict.
    pub signals: Vec<Signal>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Signal {
    pub description: String,
    pub test: String,
    pub result: String,
    pub seen: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Capability {
    pub id: String,
    pub status: String,
    pub with: Option<String>,
    pub tried: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanSummary {
    pub status: String,
    pub keeps: Vec<String>,
    pub backs_up: Vec<String>,
    pub replaces: Vec<String>,
    pub creates: Vec<String>,
    pub loses_files: Vec<String>,
    pub untouched_partitions: usize,
    pub space_at_least: u64,
    pub space_exact: bool,
}

fn vars(m: &BTreeMap<String, Option<String>>, allowed: &[&str]) -> Vars {
    let mut values = BTreeMap::new();
    let mut other_names = Vec::new();
    for (k, v) in m {
        if vocab::in_name_list(allowed, k) && !vocab::looks_unit_unique(k) {
            values.insert(k.clone(), v.clone());
        } else {
            other_names.push(k.clone());
        }
    }
    Vars { values, other_names }
}

/// The value of a fact as the report may show it, if it may.
fn seen(facts: &Snapshot, key: &str) -> Option<String> {
    if vocab::looks_unit_unique(key) {
        return None;
    }
    let (fk, param) = vocab::fact_key(key)?;
    let param = param.unwrap_or_default();
    let allowed = match fk.pattern {
        "fastboot.var.<name>" => vocab::in_name_list(vocab::REPORT_FASTBOOT_VARS, &param),
        "android.prop.<name>" => vocab::in_name_list(vocab::REPORT_ANDROID_PROPS, &param),
        // A partition's presence and size, by name only where the names are the maker's.
        "storage.partition.<name>" | "storage.partition.<name>.size" => facts.storage.as_ref().is_none_or(|s| s.names == "vendor"),
        _ => !fk.unit_unique,
    };
    if !allowed {
        return None;
    }
    match facts.observe(key, &facts.mode) {
        crate::facts::Obs::Value(v) => Some(v),
        crate::facts::Obs::Number(n) => Some(n.to_string()),
        crate::facts::Obs::Absent => Some("absent".into()),
        crate::facts::Obs::Unknown => Some("not seen in this mode".into()),
    }
}

pub fn report(ds: &[&Description], facts: &Snapshot, rec: &Recognition, plan: Option<&Plan>) -> Report {
    let vendor_names = facts.storage.as_ref().map(|s| s.names == "vendor").unwrap_or(true);
    let machine = Machine {
        usb: facts.usb.as_ref().map(|u| Usb {
            vendor_id: u.vendor_id.clone(),
            product_id: u.product_id.clone(),
            interfaces: u.interfaces.clone(),
        }),
        fastboot: facts.fastboot.as_ref().map(|f| vars(&f.variables, vocab::REPORT_FASTBOOT_VARS)),
        android: facts.android.as_ref().map(|a| vars(&a.properties, vocab::REPORT_ANDROID_PROPS)),
        disks: facts
            .storage
            .iter()
            .flat_map(|s| s.disks.iter())
            .map(|d| Disk {
                kind: d.kind.clone(),
                size: d.size,
                removable: d.removable,
                table: d.table.clone(),
                partitions: d
                    .partitions
                    .iter()
                    .map(|p| Partition {
                        name: if vendor_names {
                            p.name.clone().filter(|n| !vocab::looks_unit_unique(n))
                        } else {
                            None
                        },
                        size: p.size,
                        role: p.type_guid.as_deref().and_then(|t| {
                            if t.eq_ignore_ascii_case("c12a7328-f81f-11d2-ba4b-00a0c93ec93b") {
                                Some("esp".to_string())
                            } else {
                                None
                            }
                        }),
                        filesystem: p.fs.as_ref().map(|f| f.kind.clone()),
                    })
                    .collect(),
                free: d.free.iter().sum(),
            })
            .collect(),
        firmware: facts.firmware.as_ref().map(|f| Firmware {
            kind: f.kind.clone(),
            secure_boot: f.secure_boot.clone(),
            boot_entries: f.boot_entries.len(),
        }),
        cpu: facts.cpu.as_ref().map(|c| Cpu {
            arch: c.arch.clone(),
            vendor: c.vendor.clone(),
        }),
        board: facts.board.as_ref().map(|b| Board {
            vendor: b.vendor.clone(),
            product: b.product.clone(),
        }),
        systems: facts
            .systems
            .iter()
            .map(|s| System {
                kind: if vocab::SYSTEM_KINDS.contains(&s.kind.as_str()) {
                    s.kind.clone()
                } else {
                    "other".into()
                },
            })
            .collect(),
    };

    let desc = |id: &str| ds.iter().find(|d| d.id == id);
    let signals_of = |ids: &[String]| -> Vec<Signal> {
        rec.descriptions
            .iter()
            .filter(|r| ids.contains(&r.id))
            .flat_map(|r| {
                r.signals.iter().filter(|s| s.required).map(|s| Signal {
                    description: r.id.clone(),
                    test: s.test.clone(),
                    result: format!("{:?}", s.result).to_lowercase(),
                    seen: seen(facts, &s.key),
                })
            })
            .collect()
    };
    let mut reasons = Vec::new();
    let (result, device, state, candidates, needs, signals) = match &rec.outcome {
        Outcome::Match { id, state } => {
            let (st, needs) = match state {
                StateResult::Decided { state } => (Some(state.clone()), vec![]),
                StateResult::Undetermined { needs_modes, .. } => {
                    reasons.push(format!("the install state needs the device in {}", needs_modes.join(" or ")));
                    (None, needs_modes.clone())
                }
                StateResult::Conflict { states } => {
                    reasons.push(format!("more than one install state holds: {}", states.join(", ")));
                    (None, vec![])
                }
                StateResult::None => {
                    reasons.push("no install state holds".into());
                    (None, vec![])
                }
            };
            ("match", Some(id.clone()), st, vec![], needs, vec![])
        }
        Outcome::Conflict { ids } => {
            reasons.push("the machine's signals contradict each other".into());
            ("conflict", None, None, ids.clone(), vec![], signals_of(ids))
        }
        Outcome::Ambiguous { ids } => {
            reasons.push(format!("it matches {} and neither refines the other", ids.join(" and ")));
            ("ambiguous", None, None, ids.clone(), vec![], vec![])
        }
        Outcome::Undetermined { candidates, needs_modes } => {
            reasons.push(format!("restart the device into {} to recognise it", needs_modes.join(" or ")));
            ("undetermined", None, None, candidates.clone(), needs_modes.clone(), signals_of(candidates))
        }
        Outcome::None => {
            reasons.push("no description matches: GlassineOS doesn't support this machine yet".into());
            let near: Vec<String> = rec
                .descriptions
                .iter()
                .filter(|r| r.verdict != Verdict::NoMatch || r.signals.iter().any(|s| s.result == crate::facts::Truth::True))
                .map(|r| r.id.clone())
                .collect();
            ("none", None, None, near.clone(), vec![], signals_of(&near))
        }
    };
    let matched = device.as_deref().and_then(desc);
    let capabilities = matched
        .map(|d| {
            d.capabilities
                .iter()
                .filter(|c| c.when.is_none())
                .map(|c| Capability {
                    id: c.id.clone(),
                    status: c.status.clone(),
                    with: c.with.clone(),
                    tried: c.verified.is_some(),
                })
                .collect()
        })
        .unwrap_or_default();
    let plan_summary = plan.filter(|p| p.job.is_some()).map(|p| {
        let with = |a: &str| {
            p.partitions
                .iter()
                .filter(|x| x.actions.iter().any(|y| y == a))
                .map(|x| x.partition.clone())
                .collect::<Vec<_>>()
        };
        PlanSummary {
            status: serde_json::to_value(&p.status)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            keeps: p.systems.iter().filter(|s| s.outcome.starts_with("kept")).map(|s| s.system.clone()).collect(),
            backs_up: with("backs-up"),
            replaces: with("writes").into_iter().filter(|x| !with("creates").contains(x)).collect(),
            creates: with("creates"),
            loses_files: p.partitions.iter().filter(|x| x.loses_files).map(|x| x.partition.clone()).collect(),
            untouched_partitions: p.untouched_partitions,
            space_at_least: p.space.total_at_least,
            space_exact: p.space.exact,
        }
    });
    if let Some(p) = plan.filter(|_| matches!(rec.outcome, Outcome::Match { .. })) {
        for r in &p.reasons {
            if !reasons.contains(r) {
                reasons.push(r.clone());
            }
        }
    }
    // Supported: recognised, and a plan exists that isn't refused (001 FR-016:
    // a device lacking what GlassineOS needs to boot safely is refused).
    let supported = matches!(rec.outcome, Outcome::Match { .. }) && plan.is_some_and(|p| p.status != crate::plan::Status::Refused);
    Report {
        report_format: REPORT_FORMAT,
        mode: facts.mode.clone(),
        runs_on: facts.runs_on.as_ref().map(|r| RunsOn {
            platform: r.platform.clone(),
            connections: r.connections.clone(),
        }),
        machine,
        recognition: RecognitionReport {
            result: result.into(),
            name: matched.map(|d| d.texts.name.clone()),
            device,
            state,
            candidates,
            needs_modes: needs,
            signals,
        },
        supported,
        reasons,
        capabilities,
        plan: plan_summary,
    }
}

/// The report as plain text.
pub fn render(r: &Report) -> String {
    let mut o = String::new();
    let mut line = |s: String| {
        o.push_str(&s);
        o.push('\n');
    };
    let rec = &r.recognition;
    match &rec.name {
        Some(n) => line(format!(
            "Recognised: {n} ({}){}",
            rec.device.clone().unwrap_or_default(),
            rec.state.as_ref().map(|s| format!(", state {s}")).unwrap_or_default()
        )),
        None => line(format!("Not recognised ({})", rec.result)),
    }
    line(format!("Supported: {}", if r.supported { "yes" } else { "no" }));
    for x in &r.reasons {
        line(format!("  - {x}"));
    }
    if !rec.candidates.is_empty() {
        line(format!("Descriptions involved: {}", rec.candidates.join(", ")));
    }
    for s in &rec.signals {
        line(format!(
            "  {}: {} -> {}{}",
            s.description,
            s.test,
            s.result,
            s.seen.as_ref().map(|v| format!(" (seen: {v})")).unwrap_or_default()
        ));
    }
    line(format!("Mode: {}", r.mode));
    let m = &r.machine;
    if let Some(u) = &m.usb {
        line(format!("USB: {}:{} interfaces {}", u.vendor_id, u.product_id, u.interfaces.join(" ")));
    }
    for (what, v) in [("Boot loader variables", &m.fastboot), ("Android properties", &m.android)] {
        if let Some(v) = v {
            line(format!("{what}:"));
            for (k, x) in &v.values {
                line(format!("  {k} = {}", x.clone().unwrap_or_else(|| "(not answered)".into())));
            }
            if !v.other_names.is_empty() {
                line(format!("  also answered (values not shown): {}", v.other_names.join(", ")));
            }
        }
    }
    if let Some(f) = &m.firmware {
        line(format!(
            "Firmware: {}, secure boot {}, {} boot entries",
            f.kind,
            f.secure_boot.clone().unwrap_or_else(|| "unknown".into()),
            f.boot_entries
        ));
    }
    if let Some(c) = &m.cpu {
        line(format!(
            "Processor: {}{}",
            c.arch,
            c.vendor.as_ref().map(|v| format!(", {v}")).unwrap_or_default()
        ));
    }
    if let Some(b) = &m.board {
        line(format!(
            "System: {} {}",
            b.vendor.clone().unwrap_or_default(),
            b.product.clone().unwrap_or_default()
        ));
    }
    for (i, d) in m.disks.iter().enumerate() {
        line(format!(
            "Disk {i}: {}, {} bytes, {} partitions, {} bytes free",
            d.kind,
            d.size,
            d.partitions.len(),
            d.free
        ));
    }
    for s in &m.systems {
        line(format!("Operating system found: {}", s.kind));
    }
    if !r.capabilities.is_empty() {
        line("Capabilities:".into());
        for c in &r.capabilities {
            line(format!(
                "  {} {}{}{}",
                c.status,
                c.id,
                c.with.as_ref().map(|w| format!(" ({w})")).unwrap_or_default(),
                if c.tried { "" } else { " [untried]" }
            ));
        }
    }
    if let Some(p) = &r.plan {
        line(format!("An installation ({}):", p.status));
        line(format!("  keeps: {}", if p.keeps.is_empty() { "-".into() } else { p.keeps.join(", ") }));
        line(format!("  backs up: {}", p.backs_up.join(", ")));
        line(format!("  replaces: {}", p.replaces.join(", ")));
        line(format!("  creates: {}", p.creates.join(", ")));
        if !p.loses_files.is_empty() {
            line(format!("  deletes everything on: {}", p.loses_files.join(", ")));
        }
        line(format!("  leaves {} other partitions untouched", p.untouched_partitions));
        line(format!(
            "  space on the computer: {} {} bytes",
            if p.space_exact { "" } else { "at least" },
            p.space_at_least
        ));
    }
    o
}
