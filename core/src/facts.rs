// SPDX-License-Identifier: GPL-2.0-or-later
//! The facts snapshot: what one inspection saw, in the mode the machine was in.
//! Check and plan both compute from it. It may hold values unique to the unit
//! (inspect reads what is there); the report decides what leaves the computer.

use crate::vocab;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const FACTS_FORMAT: i64 = 1;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub facts_format: i64,
    /// The mode kind the machine was in: fastboot, android-adb, local, ...
    pub mode: String,
    /// Where the installer ran and which of the device's connections it could
    /// use from there (001 FR-019).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runs_on: Option<RunsOn>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usb: Option<UsbFacts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fastboot: Option<VarFacts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub android: Option<PropFacts>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<Storage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub firmware: Option<Firmware>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu: Option<Cpu>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board: Option<Board>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<Network>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// Operating systems inspecting found.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub systems: Vec<SystemFound>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glassine: Option<GlassineFacts>,
    /// The installer's own records on the computer for this device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installer: Option<InstallerFacts>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunsOn {
    pub platform: String,
    #[serde(default)]
    pub connections: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsbFacts {
    pub vendor_id: String,
    pub product_id: String,
    #[serde(default)]
    pub interfaces: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
}

/// Variables as asked: a value, or null when the boot loader answered FAIL. A
/// variable not listed was not asked.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VarFacts {
    pub variables: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PropFacts {
    pub properties: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Storage {
    /// Whether partition names are the maker's (an Android device's table) or
    /// labels a person may have chosen (a PC).
    pub names: String,
    pub disks: Vec<Disk>,
    /// The disk the installer runs from, when inspecting could tell. The plan
    /// never writes to it, nor puts a backup of a disk it writes on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installer_disk: Option<InstallerDisk>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallerDisk {
    pub index: u32,
    /// How it was told: `executable` (the disk holding the running program),
    /// `root` (the disk holding the running root file system) or
    /// `boot-entry` (the disk the firmware's current boot entry started from).
    pub by: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Disk {
    pub index: u32,
    pub kind: String,
    pub size: u64,
    #[serde(default)]
    pub removable: bool,
    #[serde(default)]
    pub table: Option<String>,
    #[serde(default)]
    pub guid: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    /// The logical sector size, in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sector_size: Option<u64>,
    pub partitions: Vec<Partition>,
    /// Unpartitioned regions, in bytes.
    #[serde(default)]
    pub free: Vec<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Partition {
    pub index: u32,
    #[serde(default)]
    pub name: Option<String>,
    /// Where it starts on its disk, in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<u64>,
    pub size: u64,
    #[serde(default, rename = "type")]
    pub type_guid: Option<String>,
    #[serde(default)]
    pub guid: Option<String>,
    #[serde(default)]
    pub fs: Option<Filesystem>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filesystem {
    pub kind: String,
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    /// The bytes it uses, where its superblock tells them cheaply (an
    /// estimate: a mounted file system updates its counts late).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Firmware {
    pub kind: String,
    #[serde(default)]
    pub secure_boot: Option<String>,
    #[serde(default)]
    pub boot_entries: Vec<BootEntry>,
    /// The firmware's variable store as inspecting found it: `readable`,
    /// `not-mounted` (the system hasn't made it readable) or `none`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variables: Option<String>,
    /// The entry the machine started from this time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boot_current: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub boot_order: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootEntry {
    pub number: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cpu {
    pub arch: String,
    #[serde(default)]
    pub vendor: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Board {
    #[serde(default)]
    pub vendor: Option<String>,
    #[serde(default)]
    pub product: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    #[serde(default)]
    pub uuid: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Network {
    #[serde(default)]
    pub macs: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemFound {
    /// windows, linux, macos, android, other
    pub kind: String,
    pub disk: u32,
    /// Indexes of the partitions on that disk that belong to this system.
    /// Inspect leaves out partitions systems share (the EFI system
    /// partition); the descriptions' rules protect them anyway.
    pub partitions: Vec<u32>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlassineFacts {
    pub installation: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallerFacts {
    #[serde(default)]
    pub backup_verified: bool,
    #[serde(default)]
    pub steps_done: Vec<String>,
}

/// What a fact is in the mode the snapshot was taken in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Obs {
    Value(String),
    Number(i64),
    /// Looked for and not there.
    Absent,
    /// Not observable in this mode, or not asked.
    Unknown,
}

pub fn parse_number(s: &str) -> Option<i64> {
    let s = s.trim();
    match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(h) => i64::from_str_radix(h, 16).ok(),
        None => s.parse().ok(),
    }
}

impl Snapshot {
    pub fn parse(json: &str) -> Result<Snapshot, String> {
        let s: Snapshot = serde_json::from_str(json).map_err(|e| format!("not a facts snapshot: {e}"))?;
        if s.facts_format > FACTS_FORMAT || s.facts_format < 1 {
            return Err(format!(
                "facts format {} is not one this installer reads (it reads up to {FACTS_FORMAT})",
                s.facts_format
            ));
        }
        Ok(s)
    }

    /// All partitions with a name, on every disk, with their disk.
    pub fn named_partitions(&self) -> Vec<(u32, &Partition)> {
        self.storage
            .iter()
            .flat_map(|s| s.disks.iter().flat_map(|d| d.partitions.iter().map(move |p| (d.index, p))))
            .filter(|(_, p)| p.name.is_some())
            .collect()
    }

    fn partition(&self, name: &str) -> Option<&Partition> {
        self.named_partitions().into_iter().map(|(_, p)| p).find(|p| p.name.as_deref() == Some(name))
    }

    /// A partition seen in fastboot: `partition-size:<name>` answered or not.
    fn fastboot_partition(&self, name: &str) -> Obs {
        match self.fastboot.as_ref().and_then(|f| f.variables.get(&format!("partition-size:{name}"))) {
            Some(Some(v)) => parse_number(v).map(Obs::Number).unwrap_or(Obs::Absent),
            Some(None) => Obs::Absent,
            None => Obs::Unknown,
        }
    }

    /// The observation of one fact key, in the snapshot's own mode only.
    pub fn observe(&self, key: &str, mode_kind: &str) -> Obs {
        let Some((fk, param)) = vocab::fact_key(key) else { return Obs::Unknown };
        if mode_kind != self.mode && mode_kind != vocab::INSTALLER_MODE {
            return Obs::Unknown;
        }
        if !fk.modes.contains(&mode_kind) {
            return Obs::Unknown;
        }
        let text = |v: Option<&String>| v.map(|s| Obs::Value(s.clone())).unwrap_or(Obs::Absent);
        let var = |m: Option<&BTreeMap<String, Option<String>>>, name: &str| match m.and_then(|m| m.get(name)) {
            Some(Some(v)) => Obs::Value(v.clone()),
            Some(None) => Obs::Absent,
            None => Obs::Unknown,
        };
        let param = param.unwrap_or_default();
        match fk.pattern {
            "usb.vendor-id" => self.usb.as_ref().map(|u| Obs::Value(u.vendor_id.clone())).unwrap_or(Obs::Unknown),
            "usb.product-id" => self.usb.as_ref().map(|u| Obs::Value(u.product_id.clone())).unwrap_or(Obs::Unknown),
            "usb.interface.<name>" => match &self.usb {
                Some(u) if u.interfaces.contains(&param) => Obs::Value("present".into()),
                Some(_) => Obs::Absent,
                None => Obs::Unknown,
            },
            "fastboot.var.<name>" => var(self.fastboot.as_ref().map(|f| &f.variables), &param),
            "android.prop.<name>" => var(self.android.as_ref().map(|a| &a.properties), &param),
            "storage.partition.<name>" | "storage.partition.<name>.size" => {
                let o = if self.mode == "fastboot" && self.storage.is_none() {
                    self.fastboot_partition(&param)
                } else if self.storage.is_some() {
                    self.partition(&param).map(|p| Obs::Number(p.size as i64)).unwrap_or(Obs::Absent)
                } else {
                    Obs::Unknown
                };
                match (fk.pattern.ends_with(".size"), o) {
                    (false, Obs::Number(_)) => Obs::Value("present".into()),
                    (_, o) => o,
                }
            }
            "storage.kind" => self
                .storage
                .as_ref()
                .and_then(|s| s.disks.first())
                .map(|d| Obs::Value(d.kind.clone()))
                .unwrap_or(Obs::Unknown),
            "firmware.kind" => self.firmware.as_ref().map(|f| Obs::Value(f.kind.clone())).unwrap_or(Obs::Unknown),
            "firmware.secure-boot" => self.firmware.as_ref().map(|f| text(f.secure_boot.as_ref())).unwrap_or(Obs::Unknown),
            "cpu.arch" => self.cpu.as_ref().map(|c| Obs::Value(c.arch.clone())).unwrap_or(Obs::Unknown),
            "cpu.vendor" => self.cpu.as_ref().map(|c| text(c.vendor.as_ref())).unwrap_or(Obs::Unknown),
            "board.vendor" => self.board.as_ref().map(|b| text(b.vendor.as_ref())).unwrap_or(Obs::Unknown),
            "board.product" => self.board.as_ref().map(|b| text(b.product.as_ref())).unwrap_or(Obs::Unknown),
            "glassine.installation" => match &self.glassine {
                Some(g) if g.installation => Obs::Value("present".into()),
                Some(_) => Obs::Absent,
                None => Obs::Unknown,
            },
            "installer.backup-verified" => match &self.installer {
                Some(i) if i.backup_verified => Obs::Value("present".into()),
                _ => Obs::Absent,
            },
            "installer.step.<name>" => match &self.installer {
                Some(i) if i.steps_done.contains(&param) => Obs::Value("present".into()),
                _ => Obs::Absent,
            },
            // Unit-unique keys are never read for a description.
            _ => Obs::Unknown,
        }
    }
}

/// Three-valued truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Truth {
    True,
    False,
    Undetermined,
}

/// Evaluates one condition's test against an observation.
pub fn test(c: &crate::format1::Condition, o: &Obs) -> Truth {
    use crate::format1::Scalar;
    if *o == Obs::Unknown {
        return Truth::Undetermined;
    }
    let present = !matches!(o, Obs::Absent);
    let as_num = |o: &Obs| match o {
        Obs::Number(n) => Some(*n),
        Obs::Value(v) => parse_number(v),
        _ => None,
    };
    let eq = |s: &Scalar| match (s, o) {
        (Scalar::Text(t), Obs::Value(v)) => t == v,
        (Scalar::Int(i), o) => as_num(o) == Some(*i),
        _ => false,
    };
    let b = if c.present.is_some() {
        present
    } else if c.absent.is_some() {
        !present
    } else if let Some(v) = &c.equals {
        eq(v)
    } else if let Some(vs) = &c.one_of {
        vs.iter().any(eq)
    } else if let Some(n) = c.at_least {
        as_num(o).is_some_and(|x| x >= n)
    } else if let Some(n) = c.at_most {
        as_num(o).is_some_and(|x| x <= n)
    } else {
        false
    };
    if b { Truth::True } else { Truth::False }
}
