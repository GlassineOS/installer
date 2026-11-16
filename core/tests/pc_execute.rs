// SPDX-License-Identifier: GPL-2.0-or-later
//! The PC executor on a simulated machine: sparse disks and a firmware
//! variable store, with a power cut after any write, a write that lands
//! otherwise than sent (the read-back catches it), and a cut at any save of
//! the record. The plan is the planner's, from facts the PC reader's parsers
//! make of the simulated disks. The description is the PC's, with its sizes
//! scaled down so the disks stay small (2 GiB → 40 MiB, 16 GiB → 64 MiB,
//! 8 GiB → 16 MiB).

mod common;

use common::img::{self, Part};
use common::*;
use glassine_installer::artifacts::{Artifacts, MemorySource};
use glassine_installer::efivar;
use glassine_installer::execute::{Outcome, Stopped};
use glassine_installer::facts::{Cpu, FACTS_FORMAT, GlassineFacts, InstallerDisk, RunsOn, Snapshot, Storage};
use glassine_installer::fat;
use glassine_installer::format1::Description;
use glassine_installer::lock::sha256_hex;
use glassine_installer::pc_execute::{DiskId, Machine, PcEdge, PcJob, run};
use glassine_installer::pcfacts::{self, DiskInfo, EfiVars};
use glassine_installer::plan::{Input, plan};
use glassine_installer::recognize::recognize;
use glassine_installer::record::{Confirmation, Record, StepState};
use glassine_installer::table::{self, MIB, ReadAt};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

const PAGE: u64 = 64 * 1024;
const DISK: u64 = 256 * MIB;
const STICK: u64 = 64 * MIB;

fn block<F: Future>(f: F) -> F::Output {
    pollster::block_on(f)
}

#[derive(Clone, Default)]
struct Disk {
    id: Option<DiskId>,
    pages: BTreeMap<u64, Vec<u8>>,
}

impl Disk {
    fn new(index: u32, size: u64, kind: &str) -> Disk {
        Disk {
            id: Some(DiskId {
                index,
                size,
                sector: 512,
                kind: kind.into(),
                model: Some("SIM".into()),
            }),
            pages: BTreeMap::new(),
        }
    }
    fn size(&self) -> u64 {
        self.id.as_ref().unwrap().size
    }
    fn put(&mut self, off: u64, data: &[u8]) {
        let mut at = 0usize;
        while at < data.len() {
            let o = off + at as u64;
            let (p, inside) = (o / PAGE, (o % PAGE) as usize);
            let n = (PAGE as usize - inside).min(data.len() - at);
            let page = self.pages.entry(p).or_insert_with(|| vec![0; PAGE as usize]);
            page[inside..inside + n].copy_from_slice(&data[at..at + n]);
            at += n;
        }
    }
    fn get(&self, off: u64, buf: &mut [u8]) {
        let mut at = 0usize;
        while at < buf.len() {
            let o = off + at as u64;
            let (p, inside) = (o / PAGE, (o % PAGE) as usize);
            let n = (PAGE as usize - inside).min(buf.len() - at);
            match self.pages.get(&p) {
                Some(page) => buf[at..at + n].copy_from_slice(&page[inside..inside + n]),
                None => buf[at..at + n].fill(0),
            }
            at += n;
        }
    }
    fn bytes(&self, off: u64, len: u64) -> Vec<u8> {
        let mut b = vec![0u8; len as usize];
        self.get(off, &mut b);
        b
    }
    /// The sha256 of a byte range.
    fn sha(&self, off: u64, len: u64) -> String {
        let mut h = Sha256::new();
        let mut at = 0;
        while at < len {
            let n = (len - at).min(4 * MIB);
            h.update(self.bytes(off + at, n));
            at += n;
        }
        glassine_installer::ops::hex(&h.finalize())
    }
}

impl ReadAt for Disk {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        if offset + buf.len() as u64 > self.size() {
            return Err("past the end".into());
        }
        self.get(offset, buf);
        Ok(())
    }
}

/// What goes wrong, if anything.
#[derive(Clone, Copy, Default)]
struct Faults {
    /// the power goes after the n-th write (disk or variable) is half done
    cut_at_write: Option<usize>,
    /// the n-th disk write lands with one byte flipped
    corrupt_write: Option<usize>,
}

#[derive(Clone)]
struct Sim {
    disks: Vec<Disk>,
    vars: BTreeMap<String, Vec<u8>>,
    faults: Faults,
    writes: usize,
    dead: bool,
    /// (disk, offset, len) of every disk write
    log: Vec<(u32, u64, usize)>,
}

impl Sim {
    fn alive(&self) -> Result<(), String> {
        if self.dead { Err("the power is off".into()) } else { Ok(()) }
    }
    fn disk(&mut self, i: u32) -> Result<&mut Disk, String> {
        self.disks.iter_mut().find(|d| d.id.as_ref().unwrap().index == i).ok_or("no such disk".into())
    }
    /// After a power cut: the same disks and store, on again.
    fn restarted(&self) -> Sim {
        Sim {
            faults: Faults::default(),
            dead: false,
            ..self.clone()
        }
    }
}

impl Machine for Sim {
    fn disks(&mut self) -> Result<Vec<DiskId>, String> {
        self.alive()?;
        Ok(self.disks.iter().map(|d| d.id.clone().unwrap()).collect())
    }
    fn read(&mut self, disk: u32, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        self.alive()?;
        self.disk(disk)?.read_at(offset, buf)
    }
    fn write(&mut self, disk: u32, offset: u64, data: &[u8]) -> Result<(), String> {
        self.alive()?;
        self.writes += 1;
        let n = self.writes;
        let (cut, corrupt) = (self.faults.cut_at_write == Some(n), self.faults.corrupt_write == Some(n));
        self.log.push((disk, offset, data.len()));
        let d = self.disk(disk)?;
        if offset + data.len() as u64 > d.size() {
            return Err("past the end".into());
        }
        if cut {
            d.put(offset, &data[..data.len() / 2]);
            self.dead = true;
            return Err("the power went".into());
        }
        if corrupt {
            let mut b = data.to_vec();
            let k = b.len() / 3;
            b[k] ^= 0x40;
            d.put(offset, &b);
        } else {
            d.put(offset, data);
        }
        Ok(())
    }
    fn flush(&mut self, _disk: u32) -> Result<(), String> {
        self.alive()
    }
    fn var_names(&mut self) -> Result<Vec<String>, String> {
        self.alive()?;
        Ok(self.vars.keys().cloned().collect())
    }
    fn var(&mut self, name: &str) -> Result<Option<Vec<u8>>, String> {
        self.alive()?;
        Ok(self.vars.get(name).cloned())
    }
    fn set_var(&mut self, name: &str, file: &[u8]) -> Result<(), String> {
        self.alive()?;
        self.writes += 1;
        if self.faults.cut_at_write == Some(self.writes) {
            self.dead = true;
            return Err("the power went".into());
        }
        self.vars.insert(name.into(), file.to_vec());
        Ok(())
    }
}

#[derive(Default)]
struct Edge {
    saved: Option<String>,
    saves: usize,
    cut_at_save: Option<usize>,
    kept: BTreeMap<String, Vec<u8>>,
    backup: Option<BTreeMap<String, Vec<u8>>>,
    open: Option<(String, Vec<u8>)>,
    /// flip a byte of the stored copy of this file when it is read back
    spoil_backup: Option<String>,
    rand: u8,
    notes: Vec<String>,
}

impl PcEdge for Edge {
    fn save(&mut self, record: &Record) -> Result<(), String> {
        self.saves += 1;
        if self.cut_at_save == Some(self.saves) {
            return Err("the computer went down".into());
        }
        self.saved = Some(record.to_json());
        Ok(())
    }
    fn keep(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.kept.insert(name.into(), bytes.to_vec());
        Ok(())
    }
    fn kept(&mut self, name: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self.kept.get(name).cloned())
    }
    fn backup_location(&self) -> Option<String> {
        self.backup.as_ref().map(|_| "the stick".into())
    }
    fn backup_begin(&mut self, name: &str) -> Result<(), String> {
        self.open = Some((name.into(), vec![]));
        Ok(())
    }
    fn backup_append(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.open.as_mut().unwrap().1.extend_from_slice(bytes);
        Ok(())
    }
    fn backup_end(&mut self) -> Result<(), String> {
        let (n, b) = self.open.take().unwrap();
        self.backup.as_mut().ok_or("no backup place")?.insert(n, b);
        Ok(())
    }
    fn backup_read(&mut self, name: &str, each: &mut dyn FnMut(&[u8]) -> Result<(), String>) -> Result<(), String> {
        let mut b = self.backup.as_ref().and_then(|m| m.get(name)).cloned().ok_or("not stored")?;
        if self.spoil_backup.as_deref() == Some(name) {
            b[1000] ^= 1;
        }
        each(&b)
    }
    fn backup_put(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.backup.as_mut().ok_or("no backup place")?.insert(name.into(), bytes.to_vec());
        Ok(())
    }
    fn backup_get(&mut self, name: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self.backup.as_ref().and_then(|m| m.get(name)).cloned())
    }
    fn random(&mut self) -> [u8; 16] {
        self.rand += 1;
        [self.rand; 16]
    }
    fn progress(&mut self, _: &str, _: u64, _: u64) {}
    fn note(&mut self, line: &str) {
        self.notes.push(line.into());
    }
    fn cancel_requested(&mut self) -> bool {
        false
    }
}

// ---- the machines ---------------------------------------------------------------

const GLOBAL: &str = efivar::GLOBAL;
/// The Windows ESP's unique GUID as `img::gpt` makes it (size 256 MiB, slot 1).
const WIN_ESP: &str = "00000100-0000-4000-8000-000000000001";

/// A firmware store with two entries of another system and a boot order.
fn firmware() -> BTreeMap<String, Vec<u8>> {
    let mut v = BTreeMap::new();
    v.insert(
        efivar::boot_name(0),
        img::load_option("Windows Boot Manager", Some(WIN_ESP), "\\EFI\\Microsoft\\Boot\\bootmgfw.efi"),
    );
    v.insert(efivar::boot_name(1), img::load_option("UEFI Shell", None, "\\shell.efi"));
    v.insert(efivar::global_name("BootOrder"), efivar::numbers_file(&[1, 0]));
    v.insert(format!("SecureBoot-{GLOBAL}"), vec![6, 0, 0, 0, 0]);
    v
}

fn stick() -> Disk {
    let mut s = Disk::new(1, STICK, "usb");
    s.put(0, b"the installer's stick");
    s
}

fn blank_machine() -> Sim {
    Sim {
        disks: vec![Disk::new(0, DISK, "nvme"), stick()],
        vars: firmware(),
        faults: Faults::default(),
        writes: 0,
        dead: false,
        log: vec![],
    }
}

/// A Windows-like disk: ESP (FAT32), MSR, NTFS, recovery; about 150 MiB free.
fn windows_machine() -> Sim {
    let mut d = Disk::new(0, DISK, "nvme");
    let parts = [
        Part {
            name: "EFI system partition",
            type_guid: img::ESP,
            first_lba: 2048,
            last_lba: 2048 + 40 * 2048 - 1,
        },
        Part {
            name: "Microsoft reserved partition",
            type_guid: img::MSR,
            first_lba: 41 * 2048,
            last_lba: 57 * 2048 - 1,
        },
        Part {
            name: "Basic data partition",
            type_guid: img::BASIC,
            first_lba: 57 * 2048,
            last_lba: 97 * 2048 - 1,
        },
        Part {
            name: "",
            type_guid: img::WINRE,
            first_lba: 97 * 2048,
            last_lba: 105 * 2048 - 1,
        },
    ];
    for (off, b) in img::gpt(DISK, 512, &parts) {
        d.put(off, &b);
    }
    let (esp, _) = img::fat32(40 * 2048, 100, None);
    for (off, b) in esp {
        d.put(MIB + off, &b);
    }
    d.put(MIB + 5 * MIB, b"Windows Boot Manager's files");
    for (p, size) in [(&parts[2], 40 * MIB), (&parts[3], 8 * MIB)] {
        for (off, b) in img::ntfs(size / 4096, 10, false) {
            d.put(p.first_lba * 512 + off, &b);
        }
        d.put(p.first_lba * 512 + MIB, b"the person's files");
    }
    Sim {
        disks: vec![d, stick()],
        ..blank_machine()
    }
}

// ---- facts, plan, record ------------------------------------------------------------

fn facts_of(m: &mut Sim) -> Snapshot {
    let mut disks = vec![];
    let mut layouts = vec![];
    for d in m.disks.clone() {
        let id = d.id.clone().unwrap();
        let info = DiskInfo {
            kind: id.kind.clone(),
            size: id.size,
            removable: id.kind == "usb",
            model: id.model.clone(),
            sector: 512,
        };
        let mut r = d.clone();
        let l = pcfacts::layout(&mut r, id.size, 512).ok();
        disks.push(pcfacts::disk(id.index, &info, l.as_ref()));
        layouts.push(l);
    }
    let ev = EfiVars {
        store: "readable".into(),
        vars: m.vars.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        unreadable: vec![],
    };
    let (fw, _) = pcfacts::firmware(true, &ev);
    let systems = pcfacts::systems(&disks, Some(1));
    Snapshot {
        facts_format: FACTS_FORMAT,
        mode: "local".into(),
        runs_on: Some(RunsOn {
            platform: "linux".into(),
            connections: vec!["local".into()],
        }),
        storage: Some(Storage {
            names: "user".into(),
            disks,
            installer_disk: Some(InstallerDisk {
                index: 1,
                by: "executable".into(),
            }),
        }),
        firmware: Some(fw),
        cpu: Some(Cpu {
            arch: "x86-64".into(),
            vendor: None,
        }),
        glassine: Some(GlassineFacts { installation: false }),
        systems,
        ..Default::default()
    }
}

/// The PC description with small sizes.
fn small_pc() -> String {
    let t = mutate(&pc(), "size = \"2GiB\"", "size = \"40MiB\"");
    let t = mutate(&t, "size = \"16GiB\"", "size = \"64MiB\"");
    mutate(&t, "at_least = \"8GiB\"", "at_least = \"16MiB\"")
}

fn image(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| (i as u32).wrapping_mul(2654435761).rotate_left(seed as u32) as u8).collect()
}

fn release() -> MemorySource {
    let files: [(&str, Vec<u8>, &str); 4] = [
        ("base", image(3 * MIB as usize + 512, 3), "btrfs-pool-image"),
        ("kernel", image(2 * MIB as usize + 77, 5), "efi-kernel"),
        ("initramfs", image(MIB as usize + 3, 7), "initramfs"),
        ("boot-loader", image(70_000, 9), "efi-application"),
    ];
    let mut m = serde_json::Map::new();
    let mut src = MemorySource {
        origin: "test release".into(),
        files: BTreeMap::new(),
    };
    for (role, b, kind) in files {
        let file = format!("{role}.img");
        let mut e = serde_json::json!({"file": file, "sha256": sha256_hex(&b), "size": b.len(), "kind": kind});
        if role == "base" {
            e["min_partition_bytes"] = serde_json::json!(32 * MIB);
        }
        if role == "kernel" {
            e["cmdline"] = serde_json::json!("console=ttyS0");
            e["release"] = serde_json::json!("6.18.63-0-lts");
            e["min_partition_bytes"] = serde_json::json!(20 * MIB);
        }
        m.insert(role.into(), e);
        src.files.insert(file, b);
    }
    src.files.insert(
        "release.json".into(),
        serde_json::to_vec(&serde_json::json!({"release_format": 1, "artifacts": m})).unwrap(),
    );
    src
}

struct Case {
    d: Description,
    text: String,
    plan: String,
    src: MemorySource,
}

fn case(m: &mut Sim, choices: &[(&str, &str)]) -> Case {
    let text = small_pc();
    let d = valid(&text);
    let f = facts_of(m);
    let ds = [&d];
    let rec = recognize(&ds, &f);
    let src = release();
    let a = block(Artifacts::open(src.clone())).unwrap();
    let input = Input {
        choices: choices.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        developer: true,
        release: Some(a.manifest.clone()),
        ..Default::default()
    };
    let p = plan(&ds, &f, &rec, &input);
    let plan = serde_json::to_string_pretty(&p).unwrap();
    assert!(
        matches!(
            p.status,
            glassine_installer::plan::Status::DeveloperOnly | glassine_installer::plan::Status::Ready
        ),
        "{plan}"
    );
    Case { d, text, plan, src }
}

fn record(c: &Case) -> Record {
    let a = block(Artifacts::open(c.src.clone())).unwrap();
    let arts = a.roles().iter().map(|r| a.artifact(r).unwrap()).collect();
    Record::new(
        "test",
        &c.d.id,
        c.text.as_bytes(),
        &c.plan,
        Confirmation {
            text: "install".into(),
            at: None,
        },
        arts,
        None,
    )
    .unwrap()
}

fn go(c: &Case, rec: &mut Record, m: &mut Sim, e: &mut Edge) -> Result<Outcome, Stopped> {
    let job = PcJob {
        description: &c.d,
        description_sha256: sha256_hex(c.text.as_bytes()),
        developer: true,
    };
    let mut a = block(Artifacts::open(c.src.clone())).unwrap();
    block(run(&job, &c.plan, rec, &mut a, m, e))
}

fn with_backup() -> Edge {
    Edge {
        backup: Some(BTreeMap::new()),
        ..Default::default()
    }
}

// ---- what an install leaves -------------------------------------------------------

struct Installed {
    boot: (u64, u64, String),
    system: (u64, u64, String),
    userdata: (u64, u64, String),
}

fn installed(m: &Sim, c: &Case) -> Installed {
    let mut d = m.disks[0].clone();
    let t = table::read(&mut d, DISK, 512).unwrap();
    assert!(t.primary_ok);
    let find = |n: &str| {
        let e = t
            .entries
            .iter()
            .find(|e| e.name.as_deref() == Some(n))
            .unwrap_or_else(|| panic!("no {n}: {:?}", t.entries));
        (e.start, e.size, e.guid.clone().unwrap())
    };
    let (boot, system, userdata) = (find("glassine-boot"), find("glassine-system"), find("glassine-userdata"));
    let ty = |n: &str| t.entries.iter().find(|e| e.name.as_deref() == Some(n)).unwrap().type_id.clone();
    assert_eq!(ty("glassine-boot"), table::ESP_TYPE);
    assert_eq!(ty("glassine-system"), "4f68bce3-e8cd-4db1-96e7-fbcaf984b709");
    assert_eq!(ty("glassine-userdata"), "0fc63daf-8483-4772-8e79-3d69d8477de4");
    assert_eq!(boot.1, 40 * MIB);
    assert_eq!(system.1, 64 * MIB);
    assert!(userdata.1 >= 16 * MIB);
    // the base on the system partition, byte for byte
    let base = &c.src.files["base.img"];
    assert_eq!(d.bytes(system.0, base.len() as u64), *base);
    // the boot partition: the loader at both paths, the kernel set, the records
    let vol = d.bytes(boot.0, boot.1);
    let loader = &c.src.files["boot-loader.img"];
    assert_eq!(fat::read::file(&vol, "EFI/glassine/loader.efi").as_ref(), Some(loader));
    assert_eq!(fat::read::file(&vol, "EFI/BOOT/BOOTX64.EFI").as_ref(), Some(loader));
    let lr = String::from_utf8(fat::read::file(&vol, "glassine/loader.rec").unwrap()).unwrap();
    let id = lr.lines().find_map(|l| l.strip_prefix("default=")).unwrap().to_string();
    let cmdline = lr.lines().find_map(|l| l.strip_prefix("cmdline=")).unwrap();
    assert_eq!(
        cmdline,
        format!(
            "console=ttyS0 glassine.system=PARTUUID={} glassine.userdata=PARTUUID={} glassine.img={id}",
            system.2, userdata.2
        )
    );
    assert_eq!(
        fat::read::file(&vol, &format!("glassine/{id}/vmlinuz")).as_ref(),
        Some(&c.src.files["kernel.img"])
    );
    assert_eq!(
        fat::read::file(&vol, &format!("glassine/{id}/initrd")).as_ref(),
        Some(&c.src.files["initramfs.img"])
    );
    let kr = String::from_utf8(fat::read::file(&vol, &format!("glassine/{id}/set.rec")).unwrap()).unwrap();
    assert!(kr.contains("release=6.18.63-0-lts\n") && kr.contains("seeded=1\n"), "{kr}");
    // userdata: not formatted by the installer (boot 009)
    assert_eq!(d.sha(userdata.0, 4096), d.sha(DISK - 4096 * 2, 4096), "userdata's start is as it was: zero");
    // our entry: first, the others after it in their order and unchanged
    let order = efivar::number_list(&m.vars[&efivar::global_name("BootOrder")]);
    assert_eq!(&order[1..], &[1, 0]);
    let ours = efivar::load_option(&m.vars[&efivar::boot_name(order[0])]).unwrap();
    assert_eq!(ours.label, "GlassineOS");
    assert_eq!(ours.file.as_deref(), Some("\\EFI\\glassine\\loader.efi"));
    assert_eq!(ours.partition_guid.as_deref(), Some(boot.2.as_str()));
    let fw = firmware();
    for n in [0u16, 1] {
        assert_eq!(m.vars[&efivar::boot_name(n)], fw[&efivar::boot_name(n)], "Boot{n:04X} unchanged");
    }
    // the installer's stick: never written
    assert_eq!(m.disks[1].sha(0, STICK), stick().sha(0, STICK));
    Installed { boot, system, userdata }
}

/// Everything a run leaves: the disks' pages and the variables.
type State = (Vec<BTreeMap<u64, Vec<u8>>>, BTreeMap<String, Vec<u8>>);

fn state(m: &Sim) -> State {
    (m.disks.iter().map(|d| d.pages.clone()).collect(), m.vars.clone())
}

// ---- the tests ----------------------------------------------------------------------

#[test]
fn a_blank_disk_is_installed_with_a_verified_backup() {
    let mut m = blank_machine();
    let c = case(&mut m, &[]);
    let mut rec = record(&c);
    let mut e = with_backup();
    let r = go(&c, &mut rec, &mut m, &mut e);
    assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "{r:?}\n{:#?}", e.notes);
    installed(&m, &c);
    let b = e.backup.as_ref().unwrap();
    assert_eq!(b["disk0.img"].len() as u64, DISK);
    assert_eq!(
        rec.backup.as_ref().unwrap().manifest_sha256.as_deref(),
        Some(sha256_hex(&b["manifest.json"]).as_str())
    );
    // the backup holds the disk as it was (blank) and the firmware's entries
    assert!(b["disk0.img"].iter().all(|x| *x == 0));
    let vars: BTreeMap<String, String> = serde_json::from_slice(&b["efivars.json"]).unwrap();
    assert_eq!(vars.len(), 3);
    for s in rec.plan_steps().unwrap() {
        assert_eq!(rec.state(&s), StepState::Done, "{s}");
    }
    // only the target disk was written, and only where the steps may
    assert!(m.log.iter().all(|(d, _, _)| *d == 0));
}

#[test]
fn a_kept_windows_and_its_esp_stay_byte_for_byte() {
    let mut m = windows_machine();
    let before: Vec<String> = [(MIB, 40 * MIB), (41 * MIB, 16 * MIB), (57 * MIB, 40 * MIB), (97 * MIB, 8 * MIB)]
        .iter()
        .map(|(o, l)| m.disks[0].sha(*o, *l))
        .collect();
    let c = case(&mut m, &[("backup", "off")]);
    assert!(c.plan.contains("\"outcome\": \"kept and still startable\""), "{}", c.plan);
    let mut rec = record(&c);
    let mut e = Edge::default();
    let r = go(&c, &mut rec, &mut m, &mut e);
    assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "{r:?}");
    let i = installed(&m, &c);
    let after: Vec<String> = [(MIB, 40 * MIB), (41 * MIB, 16 * MIB), (57 * MIB, 40 * MIB), (97 * MIB, 8 * MIB)]
        .iter()
        .map(|(o, l)| m.disks[0].sha(*o, *l))
        .collect();
    assert_eq!(before, after, "the ESP and Windows' partitions are unchanged");
    assert!(i.boot.0 >= 105 * MIB, "GlassineOS is in the free space after Windows");
    let mut d = m.disks[0].clone();
    let t = table::read(&mut d, DISK, 512).unwrap();
    assert_eq!(t.entries.len(), 7);
    // with the backup off, the boot settings are kept beside the record
    assert!(e.kept.contains_key("efivars-before.json"));
    assert!(rec.backup.is_none());
    // no disk write outside the table's copies and GlassineOS's partitions
    for (_, off, len) in &m.log {
        let end = off + *len as u64;
        let inside = |(s, l, _): &(u64, u64, String)| *off >= *s && end <= s + l;
        assert!(
            end <= 34 * 512 || *off >= DISK - 33 * 512 || inside(&i.boot) || inside(&i.system) || inside(&i.userdata),
            "a write at {off}"
        );
    }
}

#[test]
fn a_replaced_windows_gives_its_space_and_the_esp_stays() {
    let mut m = windows_machine();
    let esp = m.disks[0].sha(MIB, 40 * MIB);
    let c = case(&mut m, &[("systems.1", "replace"), ("backup", "off")]);
    let mut rec = record(&c);
    let mut e = Edge::default();
    let r = go(&c, &mut rec, &mut m, &mut e);
    assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "{r:?}");
    let i = installed(&m, &c);
    assert_eq!(m.disks[0].sha(MIB, 40 * MIB), esp);
    assert_eq!(i.boot.0, 41 * MIB, "GlassineOS starts where Windows' partitions were");
    let mut d = m.disks[0].clone();
    let t = table::read(&mut d, DISK, 512).unwrap();
    let names: Vec<_> = t.entries.iter().map(|e| e.name.clone().unwrap_or_default()).collect();
    assert_eq!(names, ["EFI system partition", "glassine-boot", "glassine-system", "glassine-userdata"]);
}

/// Runs a whole install once, for the reference state.
fn reference(machine: fn() -> Sim, choices: &[(&str, &str)]) -> (Case, Sim) {
    let mut m = machine();
    let c = case(&mut m.clone(), choices);
    let mut rec = record(&c);
    let mut e = with_backup();
    go(&c, &mut rec, &mut m, &mut e).unwrap();
    (c, m)
}

/// A cut after `Boot####` and before the boot order is one of the cuts:
/// the resumed run finds that entry by the boot partition's GUID and adds
/// no second one (else the machine would differ from an uncut run's).
#[test]
fn a_power_cut_in_any_write_resumes_to_the_same_machine() {
    for (machine, choices) in [
        (blank_machine as fn() -> Sim, vec![]),
        (windows_machine as fn() -> Sim, vec![("systems.1", "replace")]),
    ] {
        let (c, done) = reference(machine, &choices);
        let want = state(&done);
        let writes = done.writes;
        assert!(writes > 15, "{writes} writes");
        for k in 1..=writes {
            let mut m = machine();
            m.faults.cut_at_write = Some(k);
            let mut rec = record(&c);
            let mut e = with_backup();
            let r = go(&c, &mut rec, &mut m, &mut e);
            let s = r.expect_err("the cut stops the run");
            assert!(s.step.is_some() && s.escape.is_some(), "{s:?}");
            // the machine comes back; the installer resumes from the saved record
            let mut m = m.restarted();
            let mut rec = Record::parse(e.saved.as_ref().unwrap()).unwrap();
            let r = go(&c, &mut rec, &mut m, &mut e);
            assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "cut at write {k}: {s:?} then {r:?}");
            installed(&m, &c);
            assert!(state(&m) == want, "cut at write {k} ({s}): another machine than an uncut run's");
        }
    }
}

#[test]
fn a_cut_at_any_save_resumes_to_the_same_machine() {
    let (c, done) = reference(windows_machine, &[]);
    let want = state(&done);
    let mut k = 1;
    loop {
        let mut m = windows_machine();
        let mut rec = record(&c);
        let first = rec.clone();
        let mut e = with_backup();
        e.cut_at_save = Some(k);
        let Err(s) = go(&c, &mut rec, &mut m, &mut e) else {
            assert!(k > 8, "the run saves more than 8 times ({k})");
            break;
        };
        let mut rec = e.saved.as_ref().map(|t| Record::parse(t).unwrap()).unwrap_or(first);
        e.cut_at_save = None;
        let r = go(&c, &mut rec, &mut m, &mut e);
        assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "cut at save {k}: {s:?} then {r:?}");
        assert!(state(&m) == want, "cut at save {k}");
        k += 1;
    }
}

#[test]
fn a_write_that_reads_back_otherwise_stops_the_step_and_a_retry_finishes() {
    let (c, done) = reference(blank_machine, &[]);
    let want = state(&done);
    let disk_writes = done.log.len();
    let mut overwritten = 0;
    for k in 1..=disk_writes {
        let mut m = blank_machine();
        m.faults.corrupt_write = Some(k);
        let mut rec = record(&c);
        let mut e = with_backup();
        let s = match go(&c, &mut rec, &mut m, &mut e) {
            Err(s) => s,
            // a write a later one covers whole (the boot partition's
            // cleared boot sector, rewritten last): nothing wrong is left
            Ok(_) => {
                assert!(state(&m) == want, "corrupt write {k} went unnoticed and stayed");
                overwritten += 1;
                continue;
            }
        };
        assert!(s.why.contains("read back otherwise"), "write {k}: {s}");
        assert!(s.may_have_changed);
        let st = s.step.clone().unwrap();
        assert_eq!(rec.state(&st), StepState::Failed);
        // the failed step's escape is the description's
        assert!(s.escape.is_some());
        let r = go(&c, &mut rec, &mut m, &mut e);
        assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "write {k}: {r:?}");
        assert!(state(&m) == want, "corrupt write {k}");
    }
    assert!(overwritten <= 2, "{overwritten} of {disk_writes} corrupt writes went unnoticed");
}

#[test]
fn a_backup_that_reads_back_otherwise_stops_before_any_write() {
    let mut m = blank_machine();
    let c = case(&mut m, &[]);
    let mut rec = record(&c);
    let mut e = with_backup();
    e.spoil_backup = Some("disk0.img".into());
    let s = go(&c, &mut rec, &mut m, &mut e).unwrap_err();
    assert_eq!(s.step.as_deref(), Some("back-up"));
    assert!(s.why.contains("not verified"), "{s}");
    assert!(m.log.is_empty() && m.vars == firmware(), "nothing written");
    assert!(rec.backup.is_none());
}

#[test]
fn a_disk_changed_after_the_backup_is_not_written() {
    let mut m = windows_machine();
    let c = case(&mut m, &[]);
    let mut rec = record(&c);
    let mut e = with_backup();
    m.faults.cut_at_write = Some(1);
    go(&c, &mut rec, &mut m, &mut e).unwrap_err();
    let mut m = m.restarted();
    // another system changes the table meanwhile (a partition renamed)
    let mut h = m.disks[0].bytes(0, 34 * 512);
    let mut rec = Record::parse(e.saved.as_ref().unwrap()).unwrap();
    // put the old table back first: the cut wrote half the new backup copy
    let mut d0 = windows_machine();
    let old_tail = d0.disks[0].bytes(DISK - 33 * 512, 33 * 512);
    m.disks[0].put(DISK - 33 * 512, &old_tail);
    h[1024 + 56] ^= 1; // a name in the first entry
    let crc = table::crc32(&h[1024..1024 + 128 * 128]);
    h[512 + 88..512 + 92].copy_from_slice(&crc.to_le_bytes());
    h[512 + 16..512 + 20].fill(0);
    let hc = table::crc32(&h[512..512 + 92]);
    h[512 + 16..512 + 20].copy_from_slice(&hc.to_le_bytes());
    m.disks[0].put(0, &h);
    let n = m.log.len();
    let s = go(&c, &mut rec, &mut m, &mut e).unwrap_err();
    assert!(s.why.contains("it changed"), "{s}");
    assert_eq!(m.log.len(), n, "nothing written");
    let _ = d0.disks.pop();
}

#[test]
fn a_resume_with_another_backup_is_refused() {
    let mut m = blank_machine();
    let c = case(&mut m, &[]);
    let mut rec = record(&c);
    let mut e = with_backup();
    m.faults.cut_at_write = Some(3);
    go(&c, &mut rec, &mut m, &mut e).unwrap_err();
    let mut m = m.restarted();
    let mut rec = Record::parse(e.saved.as_ref().unwrap()).unwrap();
    e.backup.as_mut().unwrap().insert("manifest.json".into(), b"{}".to_vec());
    let n = m.log.len();
    let s = go(&c, &mut rec, &mut m, &mut e).unwrap_err();
    assert!(s.why.contains("not the backup this installation rests on"), "{s}");
    assert_eq!(m.log.len(), n);
}
