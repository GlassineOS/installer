// SPDX-License-Identifier: GPL-2.0-or-later
//! A PC's facts from what its reader gathered: each disk's table and file
//! systems (parsed here, from bytes), the operating systems they hold, and
//! the firmware's boot entries. The reader (the edge) only lists disks and
//! hands over read-only access; everything it reads is parsed here, so the
//! same code runs on image files in tests.

use crate::efivar;
use crate::facts::{BootEntry, Disk, Filesystem, Firmware, Partition, SystemFound};
use crate::fsprobe::{self, Probed};
use crate::table::{self, ESP_TYPE, Entry, Kind, ReadAt, Table};

/// A disk's layout as parsed: its table, and what is on each partition. A
/// disk without a table may hold one file system on the whole disk; it then
/// shows as partition 0 and has no free space.
#[derive(Debug, Clone)]
pub struct Layout {
    pub table: Table,
    pub filesystems: Vec<Option<Probed>>,
    pub whole: Option<Probed>,
}

pub fn layout(r: &mut dyn ReadAt, size: u64, sector: u64) -> Result<Layout, String> {
    let mut t = table::read(r, size, sector)?;
    let filesystems = t.entries.iter().map(|e| fsprobe::probe(r, e.start, e.size)).collect();
    let whole = if t.kind == Kind::None { fsprobe::probe(r, 0, size) } else { None };
    if whole.is_some() {
        t.free.clear();
    }
    Ok(Layout { table: t, filesystems, whole })
}

/// What the reader knows of a disk besides its contents.
#[derive(Debug, Clone, Default)]
pub struct DiskInfo {
    pub kind: String,
    pub size: u64,
    pub removable: bool,
    pub model: Option<String>,
    pub sector: u64,
}

pub fn disk(index: u32, info: &DiskInfo, l: Option<&Layout>) -> Disk {
    let fs = |p: &Option<Probed>| {
        p.as_ref().map(|p| Filesystem {
            kind: p.kind.clone(),
            used: p.used,
            ..Default::default()
        })
    };
    let mut d = Disk {
        index,
        kind: info.kind.clone(),
        size: info.size,
        removable: info.removable,
        model: info.model.clone(),
        sector_size: Some(info.sector).filter(|s| *s > 0),
        ..Default::default()
    };
    let Some(l) = l else {
        // Couldn't be read: no table known, so nothing on it may be planned.
        d.table = Some("unreadable".into());
        return d;
    };
    d.table = l.table.kind.name().map(String::from);
    d.partitions = l
        .table
        .entries
        .iter()
        .zip(&l.filesystems)
        .map(|(e, p)| Partition {
            index: e.index,
            name: e.name.clone(),
            start: Some(e.start),
            size: e.size,
            type_guid: Some(e.type_id.clone()),
            fs: fs(p),
            ..Default::default()
        })
        .collect();
    if l.whole.is_some() {
        d.partitions.push(Partition {
            index: 0,
            start: Some(0),
            size: info.size,
            fs: fs(&l.whole),
            ..Default::default()
        });
    }
    d.free = l.table.free.iter().map(|(_, n)| *n).collect();
    d
}

/// The partition a type id or file system says belongs to which kind of
/// operating system, if any. The EFI system partition belongs to none: every
/// system shares it.
pub fn system_kind(p: &Partition) -> Option<&'static str> {
    const WINDOWS: &[&str] = &[
        "e3c9e316-0b5c-4db8-817d-f92df00215ae", // Microsoft reserved
        "de94bba4-06d1-4d40-a16a-bfd50179d6ac", // Windows recovery
        "5808c8aa-7e8f-42e0-85d2-e1e90434cfb3", // LDM metadata
        "af9b60a0-1431-4f62-bc68-3311714a69ad", // LDM data
    ];
    const LINUX: &[&str] = &[
        "0fc63daf-8483-4772-8e79-3d69d8477de4", // Linux file system
        "4f68bce3-e8cd-4db1-96e7-fbcaf984b709", // root (x86-64)
        "b921b045-1df0-41c3-af44-4c6f280d3fae", // root (arm64)
        "44479540-f297-41b2-9af7-d131d5f0458a", // root (x86)
        "933ac7e1-2eb4-4f13-b844-0e14e2aef915", // home
        "3b8f8425-20e0-4f3b-907f-1a25a76f98e8", // srv
        "4d21b016-b534-45c2-a9fb-5c16e091fd2d", // var
        "0657fd6d-a4ab-43c4-84e5-0933c84b4f4f", // swap
        "e6d6d379-f507-44c2-a23c-238f2a3df928", // LVM
        "a19d880f-05fc-4d3b-a006-743f0f84911e", // RAID
        "bc13c2ff-59e6-4262-a352-b275fd6f7172", // extended boot loader
        "ca7d7ccb-63ed-4c53-861c-1742536059cc", // LUKS
    ];
    const MACOS: &[&str] = &[
        "48465300-0000-11aa-aa11-00306543ecac", // HFS+
        "7c3457ef-0000-11aa-aa11-00306543ecac", // APFS
        "426f6f74-0000-11aa-aa11-00306543ecac", // Apple boot
    ];
    let t = p.type_guid.as_deref().unwrap_or_default().to_ascii_lowercase();
    if t == ESP_TYPE || t == "0xef" || p.name.as_deref().is_some_and(|n| n.starts_with("glassine-")) {
        return None;
    }
    let fs = p.fs.as_ref().map(|f| f.kind.as_str()).unwrap_or_default();
    if WINDOWS.contains(&t.as_str()) || matches!(t.as_str(), "0x27" | "0x42") || fs == "ntfs" {
        return Some("windows");
    }
    if MACOS.contains(&t.as_str()) || matches!(t.as_str(), "0xaf") {
        return Some("macos");
    }
    if LINUX.contains(&t.as_str())
        || matches!(t.as_str(), "0x83" | "0x82" | "0x8e" | "0xfd")
        || matches!(fs, "ext2" | "ext3" | "ext4" | "btrfs" | "xfs" | "swap")
    {
        return Some("linux");
    }
    None
}

/// The operating systems on the disks: one per kind and disk, holding the
/// partitions of that kind. The installer's own disk holds none (it is the
/// installer, not a system to keep or replace).
pub fn systems(disks: &[Disk], installer_disk: Option<u32>) -> Vec<SystemFound> {
    let mut out: Vec<SystemFound> = Vec::new();
    for d in disks.iter().filter(|d| Some(d.index) != installer_disk) {
        for p in &d.partitions {
            let Some(kind) = system_kind(p) else { continue };
            match out.iter_mut().find(|s| s.disk == d.index && s.kind == kind) {
                Some(s) => s.partitions.push(p.index),
                None => out.push(SystemFound {
                    kind: kind.into(),
                    disk: d.index,
                    partitions: vec![p.index],
                    name: None,
                }),
            }
        }
    }
    out
}

/// The efivarfs files the reader found: (name, bytes), name without the
/// vendor GUID's suffix only for the global ones.
#[derive(Debug, Clone, Default)]
pub struct EfiVars {
    /// `none`, `not-mounted` or `readable`.
    pub store: String,
    pub vars: Vec<(String, Vec<u8>)>,
}

impl EfiVars {
    fn global(&self, name: &str) -> Option<&[u8]> {
        let full = format!("{name}-{}", efivar::GLOBAL);
        self.vars.iter().find(|(n, _)| *n == full).map(|(_, v)| v.as_slice())
    }
}

/// The firmware's facts: UEFI (with what its variables say) or not.
pub fn firmware(uefi: bool, v: &EfiVars) -> (Firmware, Vec<efivar::LoadOption>) {
    if !uefi {
        return (
            Firmware {
                kind: "bios".into(),
                ..Default::default()
            },
            vec![],
        );
    }
    let readable = v.store == "readable";
    let secure_boot = readable.then(|| {
        if v.global("SetupMode").and_then(efivar::flag) == Some(true) {
            "setup"
        } else if v.global("SecureBoot").and_then(efivar::flag) == Some(true) {
            "on"
        } else {
            "off"
        }
        .to_string()
    });
    let mut entries = Vec::new();
    let mut options = Vec::new();
    let suffix = format!("-{}", efivar::GLOBAL);
    let mut names: Vec<&(String, Vec<u8>)> = v.vars.iter().collect();
    names.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, bytes) in names {
        let Some(num) = name.strip_suffix(&suffix).and_then(|n| n.strip_prefix("Boot")) else {
            continue;
        };
        if num.len() != 4 || !num.chars().all(|c| c.is_ascii_hexdigit()) {
            continue;
        }
        let Some(o) = efivar::load_option(bytes) else { continue };
        entries.push(BootEntry {
            number: num.to_ascii_uppercase(),
            label: Some(o.label.clone()),
            path: o.file.clone(),
            active: Some(o.active),
        });
        options.push(o);
    }
    let boot_current = v.global("BootCurrent").map(efivar::numbers).and_then(|n| n.into_iter().next());
    let boot_order = v.global("BootOrder").map(efivar::numbers).unwrap_or_default();
    (
        Firmware {
            kind: "uefi".into(),
            secure_boot,
            boot_entries: entries,
            variables: Some(v.store.clone()),
            boot_current,
            boot_order,
        },
        options,
    )
}

/// The disk holding the partition with this unique GUID.
pub fn disk_of_partition_guid(layouts: &[(u32, Option<&Layout>)], g: &str) -> Option<u32> {
    layouts.iter().find_map(|(i, l)| {
        l.filter(|l| {
            l.table
                .entries
                .iter()
                .any(|e: &Entry| e.guid.as_deref().is_some_and(|x| x.eq_ignore_ascii_case(g)))
        })
        .map(|_| *i)
    })
}
