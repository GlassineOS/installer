// SPDX-License-Identifier: GPL-2.0-or-later
//! The PC check's reader: the facts of the Linux machine the installer runs
//! on, read only. It reads sysfs and procfs as text, opens each disk (or the
//! image file standing for it) read-only through [`ro`], and hands the bytes
//! to the core's parsers. It mounts nothing, starts no program, and has no
//! way to write: see `clippy.toml`, which `make lint` enforces.
//!
//! Everything is read below a root directory, `/` on a real machine. Tests
//! point it at a fake tree: `sys/block/<name>/...` as sysfs has it,
//! `dev/<name>` as an image file, `sys/firmware/efi/efivars/*` as efivarfs
//! files, `proc/self/mountinfo` and `proc/self/exe` (a symbolic link).
//!
//! It never reads a serial number, a WWID, a MAC address or a host name.

pub mod ro;

use glassine_installer::facts::{Board, Cpu, FACTS_FORMAT, GlassineFacts, InstallerDisk, RunsOn, Snapshot, Storage};
use glassine_installer::pcfacts::{self, DiskInfo, EfiVars, Layout};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// What one inspect read, and what it couldn't (a disk it may not open, a
/// value the kernel doesn't offer). The notes name no path outside the tree.
#[derive(Debug, Clone)]
pub struct Inspected {
    pub snapshot: Snapshot,
    pub notes: Vec<String>,
}

/// One block device as sysfs shows it.
#[derive(Debug, Clone)]
struct Block {
    name: String,
    info: DiskInfo,
    /// `major:minor` of the disk and of each partition, with its name.
    devs: Vec<(String, String)>,
}

struct Tree {
    root: PathBuf,
}

impl Tree {
    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel.trim_start_matches('/'))
    }
    fn text(&self, rel: &str) -> Option<String> {
        fs::read_to_string(self.path(rel)).ok().map(|s| s.trim().to_string())
    }
    fn number(&self, rel: &str) -> Option<u64> {
        self.text(rel)?.parse().ok()
    }
    fn link(&self, rel: &str) -> Option<String> {
        fs::read_link(self.path(rel)).ok().map(|p| p.to_string_lossy().into_owned())
    }
    fn names(&self, rel: &str) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(self.path(rel))
            .map(|r| r.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned())).collect())
            .unwrap_or_default();
        v.sort();
        v
    }
}

/// Block devices that are no disk a system or GlassineOS could live on.
fn skipped(name: &str) -> bool {
    const PREFIXES: &[&str] = &["loop", "ram", "zram", "dm-", "md", "sr", "nbd", "fd", "mtdblock", "zd"];
    PREFIXES.iter().any(|p| name.starts_with(p)) || (name.starts_with("mmcblk") && (name.contains("boot") || name.contains("rpmb")))
}

fn kind(t: &Tree, name: &str) -> String {
    let link = t.link(&format!("sys/block/{name}")).unwrap_or_default();
    if link.contains("/usb") {
        "usb".into()
    } else if name.starts_with("nvme") {
        "nvme".into()
    } else if name.starts_with("vd") {
        "virtio".into()
    } else if name.starts_with("mmcblk") {
        match t.text(&format!("sys/block/{name}/device/type")).as_deref() {
            Some("MMC") => "emmc".into(),
            _ => "sd".into(),
        }
    } else if name.starts_with("sd") && link.contains("/ata") {
        "sata".into()
    } else if name.starts_with("sd") {
        "scsi".into()
    } else {
        "other".into()
    }
}

fn blocks(t: &Tree) -> Vec<Block> {
    let mut out = Vec::new();
    for name in t.names("sys/block") {
        if skipped(&name) || t.number(&format!("sys/block/{name}/hidden")) == Some(1) {
            continue;
        }
        let size = t.number(&format!("sys/block/{name}/size")).unwrap_or(0) * 512;
        if size == 0 {
            continue;
        }
        let model = ["device/model", "device/name"]
            .iter()
            .find_map(|f| t.text(&format!("sys/block/{name}/{f}")))
            .filter(|m| !m.is_empty());
        let mut devs = Vec::new();
        if let Some(d) = t.text(&format!("sys/block/{name}/dev")) {
            devs.push((d, name.clone()));
        }
        for sub in t.names(&format!("sys/block/{name}")) {
            if t.path(&format!("sys/block/{name}/{sub}/partition")).is_file()
                && let Some(d) = t.text(&format!("sys/block/{name}/{sub}/dev"))
            {
                devs.push((d, sub));
            }
        }
        out.push(Block {
            info: DiskInfo {
                kind: kind(t, &name),
                size,
                removable: t.number(&format!("sys/block/{name}/removable")) == Some(1),
                model,
                sector: t.number(&format!("sys/block/{name}/queue/logical_block_size")).unwrap_or(0),
            },
            name,
            devs,
        });
    }
    out
}

/// One line of mountinfo: where, which device number, which source.
struct Mount {
    point: String,
    dev: String,
    source: String,
}

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            out.push(u8::from_str_radix(&s[i + 1..i + 4], 8).unwrap_or(b'?') as char);
            i += 4;
        } else {
            out.push(b[i] as char);
            i += 1;
        }
    }
    out
}

fn mounts(t: &Tree) -> Vec<Mount> {
    let Some(text) = t.text("proc/self/mountinfo") else { return vec![] };
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split(' ').collect();
            let sep = f.iter().position(|x| *x == "-")?;
            Some(Mount {
                point: unescape(f.get(4)?),
                dev: f.get(2)?.to_string(),
                source: unescape(f.get(sep + 2)?),
            })
        })
        .collect()
}

/// The mount a path lies on: the longest mount point that holds it, the
/// later of equal ones (it covers the earlier).
fn mount_of<'a>(ms: &'a [Mount], path: &str) -> Option<&'a Mount> {
    let within = |p: &str| p == "/" || path == p || path.starts_with(&format!("{}/", p.trim_end_matches('/')));
    ms.iter().filter(|m| within(&m.point)).max_by_key(|m| m.point.len())
}

/// The disk index holding a block device, by name (a partition, a disk, or
/// a stacked device: a loop's backing file, a device mapper's or RAID's
/// members), to a bounded depth.
fn disk_of_name(t: &Tree, ms: &[Mount], name: &str, by_name: &BTreeMap<String, u32>, depth: u32) -> Option<u32> {
    if depth > 4 {
        return None;
    }
    if let Some(i) = by_name.get(name) {
        return Some(*i);
    }
    if name.starts_with("loop") {
        let file = t.text(&format!("sys/block/{name}/loop/backing_file"))?;
        return disk_of_path(t, ms, &file, by_name, depth + 1);
    }
    // dm-* and md*: the devices under them.
    t.names(&format!("sys/block/{name}/slaves"))
        .iter()
        .find_map(|s| disk_of_name(t, ms, s, by_name, depth + 1))
}

fn disk_of_mount(t: &Tree, ms: &[Mount], m: &Mount, by_dev: &BTreeMap<String, String>, by_name: &BTreeMap<String, u32>, depth: u32) -> Option<u32> {
    let name = by_dev.get(&m.dev).cloned().or_else(|| {
        // Btrfs and overlays have a device number of their own: the source names the device.
        m.source.strip_prefix("/dev/").map(|n| n.rsplit('/').next().unwrap_or(n).to_string())
    })?;
    disk_of_name(t, ms, &name, by_name, depth)
}

fn disk_of_path(t: &Tree, ms: &[Mount], path: &str, by_name: &BTreeMap<String, u32>, depth: u32) -> Option<u32> {
    let by_dev = dev_names(t);
    let m = mount_of(ms, path)?;
    disk_of_mount(t, ms, m, &by_dev, by_name, depth)
}

/// Every block device's `major:minor` and name, loops and mappers included.
fn dev_names(t: &Tree) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for name in t.names("sys/block") {
        if let Some(d) = t.text(&format!("sys/block/{name}/dev")) {
            out.insert(d, name.clone());
        }
        for sub in t.names(&format!("sys/block/{name}")) {
            if t.path(&format!("sys/block/{name}/{sub}/partition")).is_file()
                && let Some(d) = t.text(&format!("sys/block/{name}/{sub}/dev"))
            {
                out.insert(d, sub);
            }
        }
    }
    out
}

/// Inspects the machine below `root` (`/` for the machine itself).
pub fn inspect(root: &Path) -> Result<Inspected, String> {
    let t = Tree { root: root.to_path_buf() };
    if !t.path("sys/block").is_dir() {
        return Err(format!("{} has no sys/block: not a Linux system's tree", t.root.display()));
    }
    let mut notes = Vec::new();

    // Disks: what sysfs says, and the table and file systems we parse.
    let bs = blocks(&t);
    let mut layouts: Vec<Option<Layout>> = Vec::new();
    for b in &bs {
        let l = ro::open(&t.path(&format!("dev/{}", b.name))).and_then(|mut r| pcfacts::layout(&mut r, b.info.size, b.info.sector));
        if let Err(e) = &l {
            notes.push(format!("disk {}: {}", b.name, e.replace(&t.root.display().to_string(), "")));
        }
        layouts.push(l.ok());
    }
    let disks: Vec<_> = bs
        .iter()
        .zip(&layouts)
        .enumerate()
        .map(|(i, (b, l))| pcfacts::disk(i as u32, &b.info, l.as_ref()))
        .collect();

    // The firmware: UEFI or not, and what its variables say.
    let uefi = t.path("sys/firmware/efi").is_dir();
    let vars_dir = "sys/firmware/efi/efivars";
    let mut ev = EfiVars::default();
    if uefi {
        let names = t.names(vars_dir);
        ev.store = if !t.path(vars_dir).is_dir() {
            "none"
        } else if names.is_empty() {
            "not-mounted"
        } else {
            "readable"
        }
        .into();
        let suffix = format!("-{}", glassine_installer::efivar::GLOBAL);
        for n in names {
            let Some(base) = n.strip_suffix(&suffix) else { continue };
            let wanted = matches!(base, "SecureBoot" | "SetupMode" | "BootOrder" | "BootCurrent")
                || (base.len() == 8 && base.starts_with("Boot") && base[4..].chars().all(|c| c.is_ascii_hexdigit()));
            if wanted && let Ok(bytes) = fs::read(t.path(&format!("{vars_dir}/{n}"))) {
                ev.vars.push((n, bytes));
            }
        }
        if ev.store == "not-mounted" {
            notes.push("the firmware's variables aren't readable here (efivarfs isn't mounted)".into());
        }
    }
    let (firmware, options) = pcfacts::firmware(uefi, &ev);

    // The disk the installer runs from: the one holding its program, else
    // its root file system, else the one the firmware started this time.
    let mut by_name: BTreeMap<String, u32> = BTreeMap::new();
    for (i, b) in bs.iter().enumerate() {
        for (_, n) in &b.devs {
            by_name.insert(n.clone(), i as u32);
        }
    }
    let ms = mounts(&t);
    let by_dev = dev_names(&t);
    let from_mount = |path: &str| mount_of(&ms, path).and_then(|m| disk_of_mount(&t, &ms, m, &by_dev, &by_name, 0));
    let exe = t.link("proc/self/exe");
    let mut installer_disk = None;
    if let Some(i) = exe.as_deref().and_then(from_mount) {
        installer_disk = Some(InstallerDisk {
            index: i,
            by: "executable".into(),
        });
    } else if let Some(i) = from_mount("/") {
        installer_disk = Some(InstallerDisk { index: i, by: "root".into() });
    } else if let Some(cur) = &firmware.boot_current {
        let pos = firmware.boot_entries.iter().position(|e| e.number == *cur);
        let layouts_ref: Vec<(u32, Option<&Layout>)> = layouts.iter().enumerate().map(|(i, l)| (i as u32, l.as_ref())).collect();
        if let Some(i) = pos
            .and_then(|p| options.get(p))
            .and_then(|o| o.partition_guid.as_deref())
            .and_then(|g| pcfacts::disk_of_partition_guid(&layouts_ref, g))
        {
            installer_disk = Some(InstallerDisk {
                index: i,
                by: "boot-entry".into(),
            });
        }
    }
    if installer_disk.is_none() {
        notes.push("couldn't tell which disk the installer runs from".into());
    }

    let systems = pcfacts::systems(&disks, installer_disk.as_ref().map(|d| d.index));
    let has_glassine = disks.iter().any(|d| d.partitions.iter().any(|p| p.name.as_deref() == Some("glassine-system")));

    let arch = t.text("proc/sys/kernel/arch").unwrap_or_else(|| std::env::consts::ARCH.to_string());
    let arch = match arch.as_str() {
        "x86_64" => "x86-64".to_string(),
        "aarch64" => "arm64".to_string(),
        a => a.to_string(),
    };
    let vendor = t.text("proc/cpuinfo").and_then(|c| {
        c.lines()
            .find_map(|l| l.strip_prefix("vendor_id").map(|v| v.trim_start_matches([' ', '\t', ':']).trim().to_string()))
    });
    let dmi = |f: &str| t.text(&format!("sys/class/dmi/id/{f}")).filter(|v| !v.is_empty());

    Ok(Inspected {
        snapshot: Snapshot {
            facts_format: FACTS_FORMAT,
            mode: "local".into(),
            runs_on: Some(RunsOn {
                platform: "linux".into(),
                connections: vec!["local".into()],
            }),
            storage: Some(Storage {
                names: "user".into(),
                disks,
                installer_disk,
            }),
            firmware: Some(firmware),
            cpu: Some(Cpu { arch, vendor }),
            board: Some(Board {
                vendor: dmi("sys_vendor"),
                product: dmi("product_name"),
                ..Default::default()
            }),
            systems,
            // Without a GlassineOS partition there is no installation; with
            // one, telling needs its file system read (not done here).
            glassine: (!has_glassine).then_some(GlassineFacts { installation: false }),
            ..Default::default()
        },
        notes,
    })
}
