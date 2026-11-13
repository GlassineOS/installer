// SPDX-License-Identifier: GPL-2.0-or-later
//! Live inspect over fastboot: the probe of 002 FR-012 and the reads that
//! fill a facts snapshot. It reads only: `getvar` (never `getvar:all`, which
//! no description can call safe in format 1), and `reboot` when asked (at
//! the end, or later in the session [`inspect_open`] leaves open). The
//! descriptions decide what is read; nothing here names a device.
//!
//! The probe: one short plain `getvar:product` (safe under every framing),
//! then the framing the descriptions' departures tell from that answer, then
//! the rest under that framing. A snapshot is only made when `product`, read
//! again at the end, still answers what the opening did.

use crate::facts::{self, FACTS_FORMAT, Obs, RunsOn, Snapshot, Truth, UsbFacts, VarFacts};
use crate::fastboot::{self, FbError, Framing, Reply, Session, Transport, readable_variable};
use crate::format1::{Condition, Description};
use crate::vocab;
use std::collections::{BTreeMap, BTreeSet};

/// The variables every fastboot inspect reads, besides `product` (the
/// opening) and those the descriptions name.
pub const BASE_VARIABLES: &[&str] = &[
    "variant",
    "hw-revision",
    "secure",
    "unlocked",
    "current-slot",
    "slot-count",
    "max-download-size",
];

/// The device's USB descriptor as the edge read it.
#[derive(Debug, Clone, Default)]
pub struct UsbIds {
    pub vendor_id: u16,
    pub product_id: u16,
    /// Every interface's class, subclass and protocol.
    pub interfaces: Vec<(u8, u8, u8)>,
}

impl UsbIds {
    fn facts(&self) -> UsbFacts {
        let mut interfaces: Vec<String> = Vec::new();
        for (c, s, p) in &self.interfaces {
            let i = format!("{c:02x}-{s:02x}-{p:02x}");
            if !interfaces.contains(&i) {
                interfaces.push(i);
            }
        }
        UsbFacts {
            vendor_id: format!("{:04x}", self.vendor_id),
            product_id: format!("{:04x}", self.product_id),
            interfaces,
            // Never: the serial is unique to the unit (001 Scenario 2).
            serial: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectError {
    /// The mode wasn't freshly started, or its answers stopped agreeing.
    NotFresh(String),
    /// A bounded wait ran out.
    Timeout(String),
    /// The device or the transport failed otherwise.
    Device(String),
    /// The descriptions disagree on how to talk to this answer, or a
    /// departure has parameters this backend doesn't know.
    Descriptions(String),
}

impl InspectError {
    /// A stable name for records and pages.
    pub fn kind(&self) -> &'static str {
        match self {
            InspectError::NotFresh(_) => "not-fresh",
            InspectError::Timeout(_) => "timeout",
            InspectError::Device(_) => "device",
            InspectError::Descriptions(_) => "descriptions",
        }
    }
}

impl std::fmt::Display for InspectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InspectError::NotFresh(w) => write!(
                f,
                "not a fresh start of the mode: {w}. Restart the device into the boot loader and inspect again"
            ),
            InspectError::Timeout(w) => write!(f, "the device stopped answering: {w}"),
            InspectError::Device(w) => write!(f, "the device: {w}"),
            InspectError::Descriptions(w) => write!(f, "the device descriptions: {w}"),
        }
    }
}

impl std::error::Error for InspectError {}

impl From<FbError> for InspectError {
    fn from(e: FbError) -> Self {
        match e {
            FbError::NotFresh(w) => InspectError::NotFresh(w),
            FbError::Timeout(w) => InspectError::Timeout(w),
            e => InspectError::Device(e.to_string()),
        }
    }
}

/// One row of the pre-match table: a departure that tells the framing from
/// the opening's answer, before any description is matched (FR-011, FR-012).
#[derive(Debug, Clone)]
pub struct FramingRule {
    pub description: String,
    pub mode: String,
    pub told_by: Condition,
    pub framing: Framing,
}

fn fastboot_descriptions<'a>(ds: &[&'a Description]) -> impl Iterator<Item = &'a Description> {
    ds.iter().copied().filter(|d| d.modes.iter().any(|m| m.kind == "fastboot"))
}

/// The pre-match table from every description's fastboot modes. Only a
/// `told_by` on `fastboot.var.product` can be judged from the opening; any
/// other departure applies once the device is recognised.
pub fn framing_table(ds: &[&Description]) -> Result<Vec<FramingRule>, InspectError> {
    let mut out = Vec::new();
    for d in fastboot_descriptions(ds) {
        for m in d.modes.iter().filter(|m| m.kind == "fastboot") {
            for dep in m.departures.iter().filter(|x| x.kind == "equal-length-transfers") {
                let Some(t) = dep.told_by.as_ref().filter(|t| t.key == "fastboot.var.product") else {
                    continue;
                };
                let at = format!("{} mode {}", d.id, m.id);
                let len = dep
                    .params
                    .get("length")
                    .and_then(|v| v.as_integer())
                    .filter(|n| *n > 0)
                    .ok_or_else(|| InspectError::Descriptions(format!("{at}: equal-length-transfers has no length")))?;
                match dep.params.get("warm-up").and_then(|v| v.as_str()) {
                    Some("short-command-then-dummy") => {}
                    w => return Err(InspectError::Descriptions(format!("{at}: warm-up {w:?} is not one this backend knows"))),
                }
                out.push(FramingRule {
                    description: d.id.clone(),
                    mode: m.id.clone(),
                    told_by: t.clone(),
                    framing: Framing::EqualLength { len: len as usize },
                });
            }
        }
    }
    Ok(out)
}

/// The framing for the opening's answer: the one every rule that holds for
/// it agrees on, or plain when none holds (a device no description knows).
pub fn choose_framing<'a>(table: &'a [FramingRule], product: Option<&str>) -> Result<(Framing, Vec<&'a FramingRule>), InspectError> {
    let Some(p) = product else { return Ok((Framing::Plain, vec![])) };
    let holding: Vec<&FramingRule> = table
        .iter()
        .filter(|r| facts::test(&r.told_by, &Obs::Value(p.to_string())) == Truth::True)
        .collect();
    let framings: BTreeSet<String> = holding.iter().map(|r| r.framing.name()).collect();
    if framings.len() > 1 {
        let who: Vec<String> = holding
            .iter()
            .map(|r| format!("{} mode {} ({})", r.description, r.mode, r.framing.name()))
            .collect();
        return Err(InspectError::Descriptions(format!(
            "product {p:?} tells different framings: {}",
            who.join(", ")
        )));
    }
    Ok((holding.first().map(|r| r.framing).unwrap_or(Framing::Plain), holding))
}

/// True when a fastboot mode with these USB ids needs a fresh start
/// (`fresh-boot-only`) or a warm-up (`equal-length-transfers`): then an
/// opening that isn't answered OKAY means a used mode, not an unknown device.
fn ids_need_fresh_start(ds: &[&Description], usb: &UsbFacts) -> Option<String> {
    for d in fastboot_descriptions(ds) {
        for m in d.modes.iter().filter(|m| m.kind == "fastboot") {
            let same = m.usb.as_ref().is_some_and(|u| u.vendor_id == usb.vendor_id && u.product_id == usb.product_id);
            if same && m.departures.iter().any(|x| x.kind == "equal-length-transfers" || x.kind == "fresh-boot-only") {
                return Some(format!("{} mode {}", d.id, m.id));
            }
        }
    }
    None
}

fn partition_refs(r: &str, out: &mut BTreeSet<String>) {
    // `role:...` and patterns aren't names a boot loader answers for.
    if !r.starts_with("role:") && !r.contains('*') {
        out.insert(r.to_string());
    }
}

/// What the descriptions with a fastboot mode want read: variables their
/// conditions name, and the partitions they name anywhere (rules, steps,
/// renames, conditions). Names a read may not take are left out.
pub fn wanted(ds: &[&Description]) -> (BTreeSet<String>, BTreeSet<String>) {
    let (mut vars, mut parts) = (BTreeSet::new(), BTreeSet::new());
    for d in fastboot_descriptions(ds) {
        let mut conds: Vec<&Condition> = d.identity.iter().collect();
        conds.extend(d.states.iter().flat_map(|s| s.conditions.iter()));
        conds.extend(d.steps.iter().flat_map(|s| s.done.iter()));
        conds.extend(d.modes.iter().flat_map(|m| m.departures.iter().filter_map(|x| x.told_by.as_ref())));
        for c in conds {
            if let Some((fk, Some(param))) = vocab::fact_key(&c.key) {
                match fk.pattern {
                    "fastboot.var.<name>" => {
                        vars.insert(param);
                    }
                    "storage.partition.<name>" | "storage.partition.<name>.size" => partition_refs(&param, &mut parts),
                    _ => {}
                }
            }
        }
        for rule in &d.partitions {
            rule.select.name.iter().for_each(|n| partition_refs(n, &mut parts));
            rule.select.names.iter().flatten().for_each(|n| partition_refs(n, &mut parts));
        }
        for s in &d.steps {
            for (_, list) in s.actions() {
                list.iter().for_each(|n| partition_refs(n, &mut parts));
            }
            if let Some(toml::Value::Table(r)) = s.params.get("renames") {
                for (old, new) in r {
                    partition_refs(old, &mut parts);
                    if let Some(n) = new.as_str() {
                        partition_refs(n, &mut parts);
                    }
                }
            }
            if let Some(toml::Value::String(p)) = s.params.get("partition") {
                partition_refs(p, &mut parts);
            }
        }
    }
    vars.retain(|v| v != "product" && readable_variable(v).is_ok());
    parts.retain(|p| readable_variable(&format!("partition-size:{p}")).is_ok());
    (vars, parts)
}

/// The variables an inspect reads after the opening, in the order it reads them.
pub fn reads(ds: &[&Description]) -> Vec<String> {
    let (vars, parts) = wanted(ds);
    let mut out: Vec<String> = BASE_VARIABLES.iter().map(|s| s.to_string()).collect();
    for v in vars {
        if !out.contains(&v) {
            out.push(v);
        }
    }
    for p in parts {
        out.push(format!("partition-type:{p}"));
        out.push(format!("partition-size:{p}"));
    }
    out
}

/// Where the inspect ran (001 FR-019), as the edge knows it.
#[derive(Debug, Clone)]
pub struct Host {
    pub platform: String,
    pub connections: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Inspected {
    pub snapshot: Snapshot,
    pub framing: Framing,
    /// The description modes whose departures told the framing.
    pub told_by: Vec<String>,
    /// Whether `reboot` was asked and answered OKAY.
    pub rebooted: bool,
    /// The reboot's failure, when it was asked and failed (the facts stand).
    pub reboot_error: Option<String>,
}

/// Inspects one fastboot device over `t`. Every transfer is a read but the
/// final `reboot` when `reboot` is set.
pub async fn inspect<T: Transport>(ds: &[&Description], t: T, usb: &UsbIds, host: &Host, reboot: bool) -> Result<Inspected, InspectError> {
    let (mut got, mut s) = inspect_open(ds, t, usb, host).await?;
    if reboot {
        match s.reboot().await {
            Ok(()) => got.rebooted = true,
            Err(e) => got.reboot_error = Some(e.to_string()),
        }
    }
    Ok(got)
}

/// [`inspect`] without the reboot, and the session still open: a caller that
/// restarts the device later (on the person's action) does it in this same
/// session, as a boot loader that only answers a fresh start needs. Every
/// transfer is a read; nothing is sent after the final check of `product`.
pub async fn inspect_open<T: Transport>(ds: &[&Description], mut t: T, usb: &UsbIds, host: &Host) -> Result<(Inspected, Session<T>), InspectError> {
    let usb = usb.facts();
    let table = framing_table(ds)?;
    let product = match fastboot::opening(&mut t).await? {
        Reply::Okay(p) => Some(p),
        Reply::Fail(why) => {
            if let Some(mode) = ids_need_fresh_start(ds, &usb) {
                return Err(InspectError::NotFresh(format!(
                    "the opening getvar:product was refused ({why:?}), and a device with these USB ids ({mode}) only answers in a fresh start"
                )));
            }
            None
        }
        r => return Err(InspectError::Device(format!("the opening getvar:product got {r:?}"))),
    };
    let (framing, holding) = choose_framing(&table, product.as_deref())?;
    let told_by = holding.iter().map(|r| format!("{} mode {}", r.description, r.mode)).collect();
    let mut s = Session::start(t, framing, product.clone()).await?;
    let mut variables: BTreeMap<String, Option<String>> = BTreeMap::new();
    variables.insert("product".into(), product);
    for name in reads(ds) {
        let v = s.getvar(&name).await?;
        variables.insert(name, v);
    }
    s.check_product("after the reads").await?;
    // An allow-list by construction: only what was asked, and never a name
    // that reads a value unique to the unit.
    variables.retain(|k, _| !vocab::looks_unit_unique(k));
    let got = Inspected {
        snapshot: Snapshot {
            facts_format: FACTS_FORMAT,
            mode: "fastboot".into(),
            runs_on: Some(RunsOn {
                platform: host.platform.clone(),
                connections: host.connections.clone(),
            }),
            usb: Some(usb),
            fastboot: Some(VarFacts { variables }),
            ..Default::default()
        },
        framing,
        told_by,
        rebooted: false,
        reboot_error: None,
    };
    Ok((got, s))
}
