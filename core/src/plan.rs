// SPDX-License-Identifier: GPL-2.0-or-later
//! The dry-run plan (001 FR-003, FR-005; 002 FR-014a, FR-016a, FR-023): what a
//! job would do, derived from the steps the choices select, checked against
//! the partition rules, with its space, capabilities and escapes. Planning
//! writes nothing; this module has no way to.

use crate::facts::{Snapshot, Truth, test};
use crate::format1::{Description, Scalar, Step, When};
use crate::recognize::{Outcome, Recognition, StateResult};
use crate::validate::glob;
use crate::vocab;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

pub const PLAN_FORMAT: i64 = 1;
const ESP_TYPE: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";

/// The flow's own choices, the same on every machine (001 FR-009, FR-009a;
/// 002 FR-024a): whether the plan makes the full backup, and where it goes.
/// No description has a choice with these ids.
pub const BACKUP_CHOICE: &str = "backup";
pub const BACKUP_TO_CHOICE: &str = "backup-to";
pub const RESERVED_CHOICES: &[&str] = &[BACKUP_CHOICE, BACKUP_TO_CHOICE];
/// What switching the backup off costs, as the plan states it (FR-009a).
pub const BACKUP_OFF_CONSEQUENCE: &str =
    "Without the backup there is no way back to the maker's system and no repair from the backup. The values unique to the device are still kept.";
/// What each image holds and how it is stored (001 FR-009).
pub const IMAGE_HOLDS: &str = "the partition table and every partition";
pub const IMAGE_STORED: &str = "compressed; only the used blocks where the file system tells them, every block elsewhere";

#[derive(Debug, Clone, Default)]
pub struct Input {
    /// `choice=option`, and for a template `choice.N=option` (N = the system's number).
    pub choices: BTreeMap<String, String>,
    /// Answers to the special-hardware questions: `emergency-cable=yes|no`.
    pub hardware: BTreeMap<String, bool>,
    /// A developer who asked to run untried write steps (Scenario 4).
    pub developer: bool,
    pub job: Option<String>,
    /// Free space on the computer for the backup, when known.
    pub free_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Ready,
    /// Needs an answer first (special hardware).
    NeedsAnswers,
    /// Only for a developer who asked: untried write steps.
    DeveloperOnly,
    Refused,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub plan_format: i64,
    pub description: String,
    pub device: String,
    pub mode: String,
    pub state: Option<String>,
    pub job: Option<String>,
    pub status: Status,
    pub reasons: Vec<String>,
    pub questions: Vec<Question>,
    pub choices: Vec<ChoiceMade>,
    pub systems: Vec<SystemOutcome>,
    pub partitions: Vec<PartitionPlan>,
    /// Partitions no step names and no backup image holds.
    pub untouched_partitions: usize,
    /// Partitions no step names that a backup image holds (read, never written).
    pub imaged_partitions: usize,
    pub boot_settings: Vec<BootSettingPlan>,
    pub backup: BackupPlan,
    pub keeps: Keeps,
    pub space: Space,
    pub capabilities: Vec<CapabilityPlan>,
    pub steps: Vec<StepPlan>,
    pub untried_steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Question {
    pub hardware: String,
    pub steps: Vec<String>,
    pub answer: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChoiceMade {
    pub choice: String,
    pub value: String,
    pub default: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct SystemOutcome {
    pub system: String,
    pub outcome: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PartitionPlan {
    pub partition: String,
    pub class: String,
    pub actions: Vec<String>,
    pub size: Option<u64>,
    /// A destroying change: the person's files on it are lost (FR-015).
    pub loses_files: bool,
    pub steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BootSettingPlan {
    pub id: String,
    pub kind: String,
    pub actions: Vec<String>,
    pub steps: Vec<String>,
}

/// The full backup (001 FR-009, FR-009a): one image per disk the plan writes
/// to, or the person's choice to go without.
#[derive(Debug, Clone, Serialize)]
pub struct BackupPlan {
    /// `full` (the default) or `off`.
    pub choice: String,
    pub default: bool,
    pub images: Vec<DiskImage>,
    pub target: BackupTarget,
    /// With the backup off, what that costs.
    pub consequence: Option<String>,
    /// With the backup off, the steps whose escape needed it.
    pub escapes_lost: Vec<String>,
}

impl Default for BackupPlan {
    fn default() -> Self {
        BackupPlan {
            choice: "full".into(),
            default: true,
            images: vec![],
            target: BackupTarget {
                kind: "computer".into(),
                disk: None,
            },
            consequence: None,
            escapes_lost: vec![],
        }
    }
}

/// One whole-disk image.
#[derive(Debug, Clone, Serialize)]
pub struct DiskImage {
    pub disk: String,
    pub index: u32,
    /// The disk's kind (nvme, emmc, ...) and size, when inspecting saw them.
    pub kind: Option<String>,
    pub disk_size: Option<u64>,
    pub holds: String,
    pub stored: String,
    /// The image's size as stored: `about` (the description's estimate),
    /// `at-most` (the disk's size; inspecting didn't see the used blocks) or
    /// `unknown`.
    pub estimate: String,
    pub estimate_bytes: Option<u64>,
    pub mode: String,
    pub means: String,
    pub check: String,
    pub step: String,
}

/// Where the backup goes: never a disk the plan writes to. `kind` is
/// `computer`, `installer-media` or `disk` (with its index); never a path.
#[derive(Debug, Clone, Serialize)]
pub struct BackupTarget {
    pub kind: String,
    pub disk: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Keeps {
    pub unit_values: Vec<String>,
    pub device_files: Vec<String>,
    pub device_data: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Space {
    pub backup_bytes: u64,
    /// Backed-up items whose size isn't known yet.
    pub backup_unknown: Vec<String>,
    pub artifact_bytes: u64,
    pub artifacts_unknown: Vec<String>,
    /// At least this much; exact only when both unknown lists are empty.
    pub total_at_least: u64,
    pub exact: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct CapabilityPlan {
    pub id: String,
    pub status: String,
    pub with: Option<String>,
    pub tried: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct EscapePlan {
    pub leaves: String,
    pub hardware: Option<String>,
    pub way_out: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepPlan {
    pub id: String,
    pub kind: String,
    pub mode: String,
    pub done: Truth,
    pub writes: bool,
    pub tried: bool,
    pub point_of_no_return: bool,
    pub safe_stop: bool,
    pub on_failure: EscapePlan,
    pub on_interruption: EscapePlan,
}

/// One partition (or the partition table) as the plan sees it.
#[derive(Debug, Clone)]
struct Part {
    display: String,
    /// The disk it is on.
    disk: Option<u32>,
    name: Option<String>,
    size: Option<u64>,
    type_guid: Option<String>,
    roles: BTreeSet<String>,
}

fn refused(d: Option<&Description>, facts: &Snapshot, state: Option<String>, reasons: Vec<String>) -> Plan {
    Plan {
        plan_format: PLAN_FORMAT,
        description: d.map(|d| d.id.clone()).unwrap_or_default(),
        device: d.map(|d| d.texts.name.clone()).unwrap_or_default(),
        mode: facts.mode.clone(),
        state,
        job: None,
        status: Status::Refused,
        reasons,
        questions: vec![],
        choices: vec![],
        systems: vec![],
        partitions: vec![],
        untouched_partitions: 0,
        imaged_partitions: 0,
        boot_settings: vec![],
        backup: BackupPlan::default(),
        keeps: Keeps::default(),
        space: Space::default(),
        capabilities: vec![],
        steps: vec![],
        untried_steps: vec![],
    }
}

/// An instance of a step: `for_each` steps run once per item.
struct Inst<'a> {
    id: String,
    step: &'a Step,
    item: Option<usize>,
}

pub fn plan(ds: &[&Description], facts: &Snapshot, rec: &Recognition, input: &Input) -> Plan {
    let (d, state) = match (&rec.outcome, rec.matched(ds)) {
        (
            Outcome::Match {
                state: StateResult::Decided { state },
                ..
            },
            Some(d),
        ) => (d, state.clone()),
        (
            Outcome::Match {
                state: StateResult::Undetermined { needs_modes, .. },
                ..
            },
            Some(d),
        ) => {
            return refused(
                Some(d),
                facts,
                None,
                vec![format!(
                    "the install state can't be told in this mode; restart the device into {}, or say which state it is in",
                    needs_modes.join(" or ")
                )],
            );
        }
        (Outcome::Match { state, .. }, Some(d)) => {
            return refused(
                Some(d),
                facts,
                None,
                vec![format!("the install state is unclear ({state:?}): treated as unsupported")],
            );
        }
        (Outcome::Undetermined { needs_modes, .. }, _) => {
            return refused(
                None,
                facts,
                None,
                vec![format!("not recognised in this mode; restart the device into {}", needs_modes.join(" or "))],
            );
        }
        (Outcome::Conflict { ids }, _) => {
            return refused(
                None,
                facts,
                None,
                vec![format!("signals contradict each other (descriptions {}): unsupported", ids.join(", "))],
            );
        }
        (Outcome::Ambiguous { ids }, _) => {
            return refused(
                None,
                facts,
                None,
                vec![format!("matches {} and none refines the other: unsupported", ids.join(" and "))],
            );
        }
        _ => return refused(None, facts, None, vec!["matches no description: unsupported".into()]),
    };
    let mut reasons = Vec::new();

    // The job for this state.
    let job = match &input.job {
        Some(j) => d.jobs.iter().find(|x| x.id == *j && x.states.contains(&state)),
        None => d
            .jobs
            .iter()
            .find(|x| x.id == "install" && x.states.contains(&state))
            .or_else(|| d.jobs.iter().find(|x| x.states.contains(&state))),
    };
    let Some(job) = job else {
        return refused(Some(d), facts, Some(state.clone()), vec![format!("no job is offered in state {state:?}")]);
    };

    // Choices, with their defaults.
    let mut made = Vec::new();
    let mut values: BTreeMap<String, String> = BTreeMap::new();
    for c in &d.choices {
        let keys: Vec<String> = match &c.for_each {
            Some(_) => (0..facts.systems.len()).map(|i| format!("{}.{}", c.id, i + 1)).collect(),
            None => vec![c.id.clone()],
        };
        for k in keys {
            let (v, default) = match input.choices.get(&k) {
                Some(v) => (v.clone(), false),
                None => (c.default.clone(), true),
            };
            let ok = match c.source.as_str() {
                "fixed" => c.options.iter().any(|o| o.id == v),
                _ => v == "first" || disk_index(&v).is_some_and(|n| facts.storage.as_ref().is_some_and(|s| s.disks.iter().any(|d| d.index == n))),
            };
            if !ok {
                reasons.push(format!("{v:?} is not an option of {k:?}"));
            }
            values.insert(k.clone(), v.clone());
            made.push(ChoiceMade { choice: k, value: v, default });
        }
    }
    // The flow's own choices (FR-009, FR-009a).
    let backup_to_default = if facts.mode == "local" { "installer-media" } else { "computer" };
    for (k, default) in [(BACKUP_CHOICE, "full"), (BACKUP_TO_CHOICE, backup_to_default)] {
        let (v, is_default) = match input.choices.get(k) {
            Some(v) => (v.clone(), false),
            None => (default.to_string(), true),
        };
        let ok = match k {
            BACKUP_CHOICE => v == "full" || v == "off",
            _ => v == backup_to_default || disk_index(&v).is_some_and(|n| facts.storage.as_ref().is_some_and(|s| s.disks.iter().any(|d| d.index == n))),
        };
        if !ok {
            reasons.push(format!("{v:?} is not an option of {k:?}"));
        }
        values.insert(k.to_string(), v.clone());
        made.push(ChoiceMade {
            choice: k.to_string(),
            value: v,
            default: is_default,
        });
    }
    for k in input.choices.keys() {
        if !values.contains_key(k) {
            reasons.push(format!("no choice {k:?}"));
        }
    }
    let backup_on = values.get(BACKUP_CHOICE).map(String::as_str) != Some("off");
    let holds = |w: &When, item: Option<usize>| -> bool {
        let key = match (d.choice(&w.choice).and_then(|c| c.for_each.as_ref()), item) {
            (Some(_), Some(i)) => format!("{}.{}", w.choice, i + 1),
            _ => w.choice.clone(),
        };
        values.get(&key) == Some(&w.equals)
    };

    // The target disk, for PCs.
    let target_disk: Option<u32> = d.choices.iter().find(|c| c.source == "disks").and_then(|c| {
        let v = values.get(&c.id)?;
        disk_index(v).or_else(|| facts.storage.as_ref()?.disks.iter().find(|d| !d.removable).map(|d| d.index))
    });

    // The steps the choices select, in their order.
    let mut insts: Vec<Inst> = Vec::new();
    for s in d.steps.iter().filter(|s| s.job == job.id) {
        if s.for_each.is_some() {
            for i in 0..facts.systems.len() {
                if s.when.iter().all(|w| holds(w, Some(i))) {
                    insts.push(Inst {
                        id: format!("{}.{}", s.id, i + 1),
                        step: s,
                        item: Some(i),
                    });
                }
            }
        } else if s.when.iter().all(|w| holds(w, None)) {
            insts.push(Inst {
                id: s.id.clone(),
                step: s,
                item: None,
            });
        }
    }
    let insts = order(insts);

    // Partitions as the facts show them.
    let parts = parts_of(d, facts, target_disk);
    // The disk a partition-table write or a new partition lands on: the
    // system's for a template step, else the target disk, else the first.
    let first_disk = facts.storage.as_ref().and_then(|s| s.disks.first().map(|d| d.index)).unwrap_or(0);
    let disk_for = |item: Option<usize>| -> Option<u32> { item.and_then(|i| facts.systems.get(i)).map(|s| s.disk).or(target_disk).or(Some(first_disk)) };
    let resolve = |r: &str, item: Option<usize>, creating: bool| -> Vec<Part> {
        if let Some(role) = r.strip_prefix("role:") {
            return match role {
                "partition-table" => vec![Part {
                    display: "partition table".into(),
                    disk: disk_for(item),
                    name: None,
                    size: None,
                    type_guid: None,
                    roles: ["partition-table".to_string()].into(),
                }],
                "os" => match item.and_then(|i| facts.systems.get(i)) {
                    Some(sys) => parts
                        .iter()
                        .filter(|p| sys.partitions.iter().any(|i| p.roles.contains(&format!("part:{}:{i}", sys.disk))))
                        .cloned()
                        .collect(),
                    None => vec![],
                },
                _ => parts.iter().filter(|p| p.roles.contains(role)).cloned().collect(),
            };
        }
        match parts.iter().find(|p| p.name.as_deref() == Some(r)) {
            Some(p) => vec![p.clone()],
            None if creating => vec![Part {
                display: r.to_string(),
                disk: disk_for(item),
                name: Some(r.to_string()),
                size: None,
                type_guid: None,
                roles: BTreeSet::new(),
            }],
            None => vec![Part {
                display: format!("{r} (not found)"),
                disk: disk_for(item),
                name: Some(r.to_string()),
                size: None,
                type_guid: None,
                roles: ["missing".to_string()].into(),
            }],
        }
    };
    let deciding = |p: &Part, item: Option<usize>| -> (String, Option<bool>, Option<&crate::format1::PartitionRule>) {
        for rule in &d.partitions {
            if let Some(w) = &rule.when
                && !holds(w, item)
            {
                continue;
            }
            let sel = &rule.select;
            let hit = match (&p.name, sel) {
                (_, s) if s.role.is_some() => p.roles.contains(s.role.as_deref().unwrap_or_default()),
                (_, s) if s.type_guid.is_some() => p
                    .type_guid
                    .as_deref()
                    .is_some_and(|t| t.eq_ignore_ascii_case(s.type_guid.as_deref().unwrap_or_default())),
                (Some(n), s) => {
                    s.name.as_deref() == Some(n) || s.names.as_ref().is_some_and(|x| x.contains(n)) || s.pattern.as_deref().is_some_and(|pat| glob(pat, n))
                }
                (None, _) => false,
            };
            if hit {
                return (rule.class.clone(), rule.destroys, Some(rule));
            }
        }
        ("never-write".into(), None, None)
    };

    let mut pplans: BTreeMap<String, PartitionPlan> = BTreeMap::new();
    let mut porder: Vec<String> = Vec::new();
    let mut bplans: BTreeMap<String, BootSettingPlan> = BTreeMap::new();
    let mut steps = Vec::new();
    let mut untried = Vec::new();
    let mut hw: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut backup_bytes = 0u64;
    let mut backup_unknown = BTreeSet::new();
    let mut artifacts_used = BTreeSet::new();
    let mut created: BTreeSet<String> = BTreeSet::new();
    // The disks the plan writes to, and the step that images them.
    let mut touched_disks: BTreeSet<u32> = BTreeSet::new();
    let mut disk_step: Option<String> = None;
    for inst in &insts {
        let s = inst.step;
        created.extend(s.creates.iter().cloned());
        let done = all_true(s.done.iter().map(|c| {
            let kind = if c.mode.as_deref() == Some(vocab::INSTALLER_MODE) {
                vocab::INSTALLER_MODE.to_string()
            } else {
                d.mode(c.mode.as_deref().unwrap_or_default()).map(|m| m.kind.clone()).unwrap_or_default()
            };
            test(c, &facts.observe(&c.key, &kind))
        }));
        for (action, list) in s.actions() {
            for r in list {
                if r == "role:disk" {
                    // Whole disks: which ones is known once every step is seen.
                    if action == "backs_up" {
                        disk_step.get_or_insert(inst.id.clone());
                    } else {
                        reasons.push(format!("step {:?} {} a whole disk, which no step may", inst.id, action.replace('_', " ")));
                    }
                    continue;
                }
                for p in resolve(r, inst.item, action == "creates" || s.creates.contains(r) || created.contains(r)) {
                    if p.roles.contains("missing") && action != "creates" {
                        reasons.push(format!(
                            "step {:?} {} {}, which inspecting didn't find",
                            inst.id,
                            action.replace('_', " "),
                            p.display
                        ));
                    }
                    let (class, destroys, rule) = deciding(&p, inst.item);
                    if !vocab::class_allows(&class, action) {
                        reasons.push(format!(
                            "step {:?} goes beyond the rule for {}: it is {class}, which doesn't allow {}",
                            inst.id,
                            p.display,
                            action.replace('_', " ")
                        ));
                    }
                    if action == "backs_up" {
                        match p.size.or_else(|| {
                            rule.and_then(|r| r.back_up.as_ref()).and_then(|b| match &b.size {
                                Some(Scalar::Int(n)) => Some(*n as u64),
                                _ => None,
                            })
                        }) {
                            Some(n) => backup_bytes += n,
                            None => {
                                backup_unknown.insert(p.display.clone());
                            }
                        }
                    }
                    let e = pplans.entry(p.display.clone()).or_insert_with(|| {
                        porder.push(p.display.clone());
                        PartitionPlan {
                            partition: p.display.clone(),
                            class: class.clone(),
                            actions: vec![],
                            size: p.size,
                            loses_files: false,
                            steps: vec![],
                        }
                    });
                    if matches!(action, "writes" | "creates" | "resizes" | "removes")
                        && let Some(n) = p.disk
                    {
                        touched_disks.insert(n);
                    }
                    let a = action.replace('_', "-");
                    if !e.actions.contains(&a) {
                        e.actions.push(a);
                    }
                    if !e.steps.contains(&inst.id) {
                        e.steps.push(inst.id.clone());
                    }
                    if matches!(action, "resizes" | "removes") && destroys == Some(true) {
                        e.loses_files = true;
                    }
                }
            }
        }
        for b in &s.boot_settings {
            if let Some(bs) = d.boot_settings.iter().find(|x| x.id == *b) {
                let e = bplans.entry(b.clone()).or_insert_with(|| BootSettingPlan {
                    id: b.clone(),
                    kind: bs.kind.clone(),
                    actions: vec![],
                    steps: vec![],
                });
                let a = if s.kind == "back-up" { "back-up" } else { "change" }.to_string();
                if !e.actions.contains(&a) {
                    e.actions.push(a);
                }
                e.steps.push(inst.id.clone());
                if s.kind == "back-up" && bs.back_up.with.is_none() {
                    match &bs.back_up.size {
                        Some(Scalar::Int(n)) => backup_bytes += *n as u64,
                        _ => {
                            backup_unknown.insert(format!("boot setting {b}"));
                        }
                    }
                }
            }
        }
        if let Some(toml::Value::String(role)) = s.params.get("artifact") {
            artifacts_used.insert(role.clone());
        }
        let tried = s.verified.is_some();
        if !tried && s.writes_anything() && done != Truth::True {
            untried.push(inst.id.clone());
        }
        for e in [&s.escape.failure, &s.escape.interruption] {
            if let Some(h) = &e.hardware {
                let v = hw.entry(h.clone()).or_default();
                if !v.contains(&inst.id) {
                    v.push(inst.id.clone());
                }
            }
        }
        let esc = |e: &crate::format1::EscapeCase| EscapePlan {
            leaves: e.leaves.clone(),
            hardware: e.hardware.clone(),
            way_out: match (&e.way_out.step, &e.way_out.transition) {
                (Some(st), _) => format!("step {st}"),
                (_, Some(t)) => format!("transition {t}"),
                _ => String::new(),
            },
        };
        steps.push(StepPlan {
            id: inst.id.clone(),
            kind: s.kind.clone(),
            mode: s.mode.clone(),
            done,
            writes: s.writes_anything(),
            tried,
            point_of_no_return: s.point_of_no_return,
            safe_stop: s.safe_stop,
            on_failure: esc(&s.escape.failure),
            on_interruption: esc(&s.escape.interruption),
        });
    }

    // The full backup (001 FR-009): one image per disk the plan writes to.
    let mut backup = BackupPlan {
        choice: values.get(BACKUP_CHOICE).cloned().unwrap_or_else(|| "full".into()),
        default: !input.choices.contains_key(BACKUP_CHOICE),
        ..BackupPlan::default()
    };
    let to = values.get(BACKUP_TO_CHOICE).cloned().unwrap_or_default();
    backup.target = match disk_index(&to) {
        Some(n) => BackupTarget {
            kind: "disk".into(),
            disk: Some(n),
        },
        None => BackupTarget { kind: to, disk: None },
    };
    let disk_rule = d.partitions.iter().find(|r| r.select.role.as_deref() == Some("disk"));
    let disk_means = disk_rule.and_then(|r| r.back_up.as_ref()).filter(|b| b.means.is_some());
    if backup_on && !touched_disks.is_empty() {
        match (disk_means, &disk_step) {
            (Some(b), Some(step)) => {
                for n in &touched_disks {
                    let disk = facts.storage.as_ref().and_then(|s| s.disks.iter().find(|x| x.index == *n));
                    let (estimate, bytes) = match (&b.size, disk) {
                        (Some(Scalar::Int(x)), _) => ("about", Some(*x as u64)),
                        (_, Some(disk)) => ("at-most", Some(disk.size)),
                        _ => ("unknown", None),
                    };
                    match bytes {
                        Some(x) => backup_bytes += x,
                        None => {
                            backup_unknown.insert(format!("disk {n} image"));
                        }
                    }
                    backup.images.push(DiskImage {
                        disk: format!("disk {n}"),
                        index: *n,
                        kind: disk.map(|x| x.kind.clone()),
                        disk_size: disk.map(|x| x.size),
                        holds: IMAGE_HOLDS.into(),
                        stored: IMAGE_STORED.into(),
                        estimate: estimate.into(),
                        estimate_bytes: bytes,
                        mode: b.mode.clone().unwrap_or_default(),
                        means: b.means.clone().unwrap_or_default(),
                        check: b.check.clone().unwrap_or_default(),
                        step: step.clone(),
                    });
                }
                if let Some(n) = backup.target.disk
                    && touched_disks.contains(&n)
                {
                    reasons.push(format!(
                        "the backup can't go to disk {n}: the plan writes to it; choose another place or switch the backup off"
                    ));
                }
            }
            _ => reasons.push(
                "the description doesn't say how to back up a whole disk, so the full backup can't be made; switch the backup off to install without one"
                    .into(),
            ),
        }
    }
    if !backup_on {
        backup.consequence = Some(BACKUP_OFF_CONSEQUENCE.into());
        backup.escapes_lost = steps
            .iter()
            .filter(|s| s.on_failure.leaves == "installer-can-undo" || s.on_interruption.leaves == "installer-can-undo")
            .map(|s| s.id.clone())
            .collect();
    }

    // Space (FR-016a: the backup counts, as well as the artifacts).
    let mut artifact_bytes = 0;
    let mut artifacts_unknown = Vec::new();
    for role in &artifacts_used {
        match d.artifacts.iter().find(|a| a.role == *role).map(|a| &a.size) {
            Some(Scalar::Int(n)) => artifact_bytes += *n as u64,
            _ => artifacts_unknown.push(role.clone()),
        }
    }
    let backup_unknown: Vec<String> = backup_unknown.into_iter().collect();
    let space = Space {
        backup_bytes,
        // An image's size is an estimate until it is made.
        exact: backup_unknown.is_empty() && artifacts_unknown.is_empty() && backup.images.is_empty(),
        backup_unknown,
        artifact_bytes,
        artifacts_unknown,
        total_at_least: backup_bytes + artifact_bytes,
    };
    if let Some(free) = input.free_bytes
        && space.total_at_least > free
    {
        reasons.push(format!(
            "not enough space on the computer: needs at least {} bytes, has {free}",
            space.total_at_least
        ));
    }

    // Capabilities (001 FR-016).
    let mut caps = Vec::new();
    for c in &d.capabilities {
        if c.when.as_ref().is_some_and(|w| !holds(w, None)) {
            continue;
        }
        if c.status != "have" && vocab::CAPABILITIES.iter().any(|v| v.id == c.id && v.safe_boot) {
            reasons.push(format!("lacks {}, which GlassineOS needs to boot safely", c.id));
        }
        caps.push(CapabilityPlan {
            id: c.id.clone(),
            status: c.status.clone(),
            with: c.with.clone(),
            tried: c.verified.is_some(),
        });
    }

    // What happens to each system.
    let mut systems = Vec::new();
    if let Some(c) = caps.iter().find(|c| c.id == "keep-stock") {
        systems.push(SystemOutcome {
            system: "stock system".into(),
            outcome: if c.status == "have" {
                "kept and still startable".into()
            } else {
                "not kept".into()
            },
        });
    }
    for (i, sys) in facts.systems.iter().enumerate() {
        let replaced = d.choices.iter().filter(|c| c.for_each.as_deref() == Some("systems")).any(|c| {
            values
                .get(&format!("{}.{}", c.id, i + 1))
                .is_some_and(|v| c.options.iter().any(|o| o.id == *v && o.replaces_system))
        });
        systems.push(SystemOutcome {
            system: format!("system {} ({})", i + 1, sys.kind),
            outcome: if replaced { "replaced".into() } else { "kept and still startable".into() },
        });
    }

    // Questions, and who may run it.
    let mut questions = Vec::new();
    for (h, st) in hw {
        let answer = input.hardware.get(&h).copied();
        if answer == Some(false) {
            reasons.push(format!("steps {} can only be escaped with the {h}, which you don't have", st.join(", ")));
        }
        questions.push(Question {
            hardware: h,
            steps: st,
            answer,
        });
    }
    let status = if !reasons.is_empty() {
        Status::Refused
    } else if questions.iter().any(|q| q.answer.is_none()) {
        Status::NeedsAnswers
    } else if !untried.is_empty() && !input.developer {
        Status::DeveloperOnly
    } else {
        Status::Ready
    };
    if status == Status::DeveloperOnly {
        reasons.push(format!(
            "steps {} write and haven't been tried on real hardware: only a developer who asks for them may run them",
            untried.join(", ")
        ));
    }

    let touched: BTreeSet<&str> = pplans.keys().map(String::as_str).collect();
    let imaged: BTreeSet<u32> = backup.images.iter().map(|i| i.index).collect();
    let (mut untouched, mut imaged_parts) = (0, 0);
    for p in parts.iter().filter(|p| !touched.contains(p.display.as_str())) {
        match p.disk {
            Some(n) if imaged.contains(&n) => {
                if !p.roles.contains("free-space") {
                    imaged_parts += 1;
                }
            }
            _ => untouched += 1,
        }
    }
    Plan {
        plan_format: PLAN_FORMAT,
        description: d.id.clone(),
        device: d.texts.name.clone(),
        mode: facts.mode.clone(),
        state: Some(state),
        job: Some(job.id.clone()),
        status,
        reasons,
        questions,
        choices: made,
        systems,
        partitions: porder.iter().filter_map(|k| pplans.remove(k)).collect(),
        untouched_partitions: untouched,
        imaged_partitions: imaged_parts,
        boot_settings: bplans.into_values().collect(),
        backup,
        keeps: Keeps {
            unit_values: d.unit_values.iter().map(|u| u.what.clone()).collect(),
            device_files: d.device_files.iter().map(|f| f.what.clone()).collect(),
            device_data: d.device_data.clone(),
        },
        space,
        capabilities: caps,
        steps,
        untried_steps: untried,
    }
}

fn disk_index(v: &str) -> Option<u32> {
    v.strip_prefix("disk").and_then(|n| n.parse().ok())
}

fn all_true(it: impl Iterator<Item = Truth>) -> Truth {
    let mut r = Truth::True;
    for t in it {
        match t {
            Truth::False => return Truth::False,
            Truth::Undetermined => r = Truth::Undetermined,
            Truth::True => {}
        }
    }
    r
}

/// Orders step instances by `after` (the list's own order only breaks ties).
fn order(insts: Vec<Inst>) -> Vec<Inst> {
    let mut left = insts;
    let mut out: Vec<Inst> = Vec::new();
    while !left.is_empty() {
        let ready = left.iter().position(|i| {
            i.step
                .after
                .iter()
                .all(|a| out.iter().any(|o| o.step.id == *a) || !left.iter().any(|l| l.step.id == *a))
        });
        let at = ready.unwrap_or(0);
        out.push(left.remove(at));
    }
    out
}

/// The partitions inspecting found, with the roles the plan resolves.
fn parts_of(d: &Description, facts: &Snapshot, target: Option<u32>) -> Vec<Part> {
    let mut out = Vec::new();
    let vendor_names = facts.storage.as_ref().map(|s| s.names == "vendor").unwrap_or(true);
    if let Some(st) = &facts.storage {
        for disk in &st.disks {
            for p in &disk.partitions {
                let mut roles = BTreeSet::new();
                roles.insert(format!("part:{}:{}", disk.index, p.index));
                let on_target = target.is_none_or(|t| t == disk.index);
                if p.type_guid.as_deref().is_some_and(|t| t.eq_ignore_ascii_case(ESP_TYPE)) && on_target {
                    roles.insert("esp".into());
                }
                if facts.systems.iter().any(|s| s.disk == disk.index && s.partitions.contains(&p.index)) {
                    roles.insert("os".into());
                }
                let display = if vendor_names {
                    p.name.clone().unwrap_or_else(|| format!("disk {} partition {}", disk.index, p.index))
                } else {
                    format!("disk {} partition {}", disk.index, p.index)
                };
                out.push(Part {
                    display,
                    disk: Some(disk.index),
                    name: p.name.clone(),
                    size: Some(p.size),
                    type_guid: p.type_guid.clone(),
                    roles,
                });
            }
            if target == Some(disk.index) {
                let free: u64 = disk.free.iter().sum();
                out.push(Part {
                    display: format!("disk {} free space", disk.index),
                    disk: Some(disk.index),
                    name: None,
                    size: Some(free),
                    type_guid: None,
                    roles: ["free-space".to_string()].into(),
                });
            }
        }
    } else if let Some(fb) = &facts.fastboot {
        for (k, v) in &fb.variables {
            if let (Some(name), Some(v)) = (k.strip_prefix("partition-size:"), v) {
                // Fixed partitions a boot loader names are on its one disk.
                out.push(Part {
                    display: name.into(),
                    disk: Some(0),
                    name: Some(name.into()),
                    size: crate::facts::parse_number(v).map(|n| n as u64),
                    type_guid: None,
                    roles: BTreeSet::new(),
                });
            }
        }
    }
    let _ = d;
    out
}

fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut x = n as f64;
    let mut u = 0;
    while x >= 1024.0 && u < U.len() - 1 {
        x /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{x:.1} {}", U[u]) }
}

/// The plan as plain text, for a terminal or a log.
pub fn render(p: &Plan, d: Option<&Description>) -> String {
    let mut o = String::new();
    let line = |o: &mut String, s: String| {
        o.push_str(&s);
        o.push('\n');
    };
    let text = |k: &str| d.and_then(|d| d.texts.steps.get(k).cloned());
    line(
        &mut o,
        format!("Plan (dry run) for {}", if p.device.is_empty() { "an unrecognised machine" } else { &p.device }),
    );
    if let Some(s) = &p.state {
        line(&mut o, format!("State: {s}. Job: {}.", p.job.clone().unwrap_or_default()));
    }
    line(
        &mut o,
        format!(
            "Status: {}",
            match p.status {
                Status::Ready => "ready",
                Status::NeedsAnswers => "needs answers",
                Status::DeveloperOnly => "developers only",
                Status::Refused => "refused",
            }
        ),
    );
    for r in &p.reasons {
        line(&mut o, format!("  - {r}"));
    }
    if !p.questions.is_empty() {
        line(&mut o, "\nQuestions:".into());
        for q in &p.questions {
            let a = match q.answer {
                None => "not answered",
                Some(true) => "yes",
                Some(false) => "no",
            };
            line(
                &mut o,
                format!(
                    "  Do you have the {}? Only it gets the device out if step {} goes wrong. Answer: {a}",
                    q.hardware,
                    q.steps.join(", ")
                ),
            );
        }
    }
    if !p.choices.is_empty() {
        line(&mut o, "\nChoices:".into());
        for c in &p.choices {
            line(&mut o, format!("  {} = {}{}", c.choice, c.value, if c.default { " (default)" } else { "" }));
        }
    }
    if !p.systems.is_empty() {
        line(&mut o, "\nSystems:".into());
        for s in &p.systems {
            line(&mut o, format!("  {}: {}", s.system, s.outcome));
        }
    }
    if !p.partitions.is_empty() {
        line(&mut o, "\nPartitions:".into());
        for x in &p.partitions {
            let size = x.size.map(human).map(|s| format!(", {s}")).unwrap_or_default();
            line(&mut o, format!("  {} ({}{size}): {}", x.partition, x.class, x.actions.join(", ")));
            if x.loses_files {
                line(&mut o, "    Everything on it is deleted: files on it are lost.".into());
            }
        }
        if p.imaged_partitions > 0 {
            line(
                &mut o,
                format!(
                    "  {} other partitions are only read, into the backup; {} are not touched.",
                    p.imaged_partitions, p.untouched_partitions
                ),
            );
        } else {
            line(&mut o, format!("  {} other partitions are not touched.", p.untouched_partitions));
        }
    }
    if p.job.is_some() {
        let b = &p.backup;
        let target = match (b.target.kind.as_str(), b.target.disk) {
            ("disk", Some(n)) => format!("disk {n}"),
            ("installer-media", _) => "the installer's USB stick".into(),
            ("computer", _) => "this computer".into(),
            (k, _) => k.to_string(),
        };
        if b.choice == "off" {
            line(&mut o, "\nBackup: off (your choice).".into());
            if let Some(c) = &b.consequence {
                line(&mut o, format!("  {c}"));
            }
            if !b.escapes_lost.is_empty() {
                line(&mut o, format!("  Steps whose way out needed it: {}.", b.escapes_lost.join(", ")));
            }
        } else if !b.images.is_empty() {
            line(
                &mut o,
                format!(
                    "\nBackup{}, to {target}, checked by reading it back before the first write:",
                    if b.default { " (default)" } else { "" }
                ),
            );
            for i in &b.images {
                let what = match (&i.kind, i.disk_size) {
                    (Some(k), Some(n)) => format!(" ({k}, {})", human(n)),
                    _ => String::new(),
                };
                let size = match (i.estimate.as_str(), i.estimate_bytes) {
                    ("about", Some(n)) => format!("about {}", human(n)),
                    ("at-most", Some(n)) => format!("at most {}", human(n)),
                    _ => "size unknown".into(),
                };
                line(&mut o, format!("  {}{what}, whole: {}; {size}, stored {}", i.disk, i.holds, i.stored));
            }
        }
    }
    if !p.boot_settings.is_empty() {
        line(&mut o, "\nHow the machine starts:".into());
        for b in &p.boot_settings {
            line(&mut o, format!("  {} ({}): {}", b.id, b.kind, b.actions.join(", ")));
        }
    }
    let k = &p.keeps;
    if !(k.unit_values.is_empty() && k.device_files.is_empty() && k.device_data.is_empty()) {
        line(&mut o, "\nKept from the device:".into());
        for v in &k.unit_values {
            line(&mut o, format!("  value: {v}"));
        }
        for v in &k.device_files {
            line(&mut o, format!("  vendor file (used, then deleted from the computer): {v}"));
        }
        for v in &k.device_data {
            line(&mut o, format!("  device data: {v}"));
        }
    }
    let s = &p.space;
    if p.job.is_some() {
        line(&mut o, "\nSpace on the computer:".into());
        line(
            &mut o,
            format!(
                "  backup {}{}",
                human(s.backup_bytes),
                if s.backup_unknown.is_empty() {
                    String::new()
                } else {
                    format!(" plus {} of unknown size", s.backup_unknown.join(", "))
                }
            ),
        );
        line(
            &mut o,
            format!(
                "  artifacts {}{}",
                human(s.artifact_bytes),
                if s.artifacts_unknown.is_empty() {
                    String::new()
                } else {
                    format!(" plus {} of unknown size", s.artifacts_unknown.join(", "))
                }
            ),
        );
        line(&mut o, format!("  {} {}", if s.exact { "total" } else { "at least" }, human(s.total_at_least)));
    }
    if !p.capabilities.is_empty() {
        line(&mut o, "\nAfterwards the device:".into());
        for c in &p.capabilities {
            let w = c.with.as_ref().map(|w| format!(" ({w})")).unwrap_or_default();
            line(&mut o, format!("  {} {}{w}{}", c.status, c.id, if c.tried { "" } else { " [untried]" }));
        }
    }
    if !p.steps.is_empty() {
        line(&mut o, "\nSteps:".into());
        for (n, st) in p.steps.iter().enumerate() {
            let flags = [
                (!st.tried && st.writes, "untried"),
                (st.point_of_no_return, "point of no return"),
                (st.safe_stop, "safe stop"),
                (st.done == Truth::True, "already done"),
            ]
            .iter()
            .filter(|(b, _)| *b)
            .map(|(_, s)| *s)
            .collect::<Vec<_>>();
            let flags = if flags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", flags.join(", "))
            };
            line(&mut o, format!("  {}. {} ({}, in {}){flags}", n + 1, st.id, st.kind, st.mode));
            let base = st.id.split('.').next().unwrap_or_default();
            if let Some(t) = text(base) {
                line(&mut o, format!("     {t}"));
            }
            let esc = |e: &EscapePlan| {
                format!(
                    "{}{} via {}",
                    e.leaves,
                    e.hardware.as_ref().map(|h| format!(" ({h})")).unwrap_or_default(),
                    e.way_out
                )
            };
            let lost = |e: &EscapePlan| {
                if p.backup.choice == "off" && e.leaves == "installer-can-undo" {
                    " (not possible: the backup is off)"
                } else {
                    ""
                }
            };
            line(&mut o, format!("     if it fails: {}{}", esc(&st.on_failure), lost(&st.on_failure)));
            line(
                &mut o,
                format!("     if cut off after it: {}{}", esc(&st.on_interruption), lost(&st.on_interruption)),
            );
        }
    }
    o
}
