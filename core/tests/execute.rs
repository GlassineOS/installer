// SPDX-License-Identifier: GPL-2.0-or-later
//! The executor (`execute`) running the Note Air's whole install job against
//! a simulated device: its boot loaders (tests/common/fbsim.rs) and the RAM
//! environment of protocol version 2 (tests/common/ramsim.rs) over one
//! eMMC that starts with the stock partition table of
//! `fixtures/gpt/noteair-stock.gpt` and ends with the A/B one.
//!
//! The stock table is the fixture's with one change: vendor is 8 MiB instead
//! of 800 MiB (its last sector moved down, the CRCs redone), so slot B's
//! vendor copy stays quick. Everything else is the real geometry (a 29 GiB
//! disk held sparsely).
//!
//! Spec 001 SC-003: the run is cut at every save of the record (before a
//! step's intent is kept, and after its write but before its outcome is
//! kept), then resumed from the record that was saved; every resume ends
//! with the same device as an uncut run.

mod common;
use common::bootimg::boot_image;
use common::fbsim::FbSim;
use common::ramsim::{CHUNK, RamEnv, Sparse, WriteFaults, image};
use common::{facts, noteair, valid};
use glassine_installer::artifacts::{Artifacts, MemorySource};
use glassine_installer::boot_patch::{Patched, patch_vendor_slotselect};
use glassine_installer::execute::{InstallEdge, Job, Link, Outcome, Reach, Stopped, run};
use glassine_installer::fastboot::{FbError, Transport};
use glassine_installer::format1::{Description, Mode, Step};
use glassine_installer::lock::sha256_hex;
use glassine_installer::ops::{self, parse_gpt};
use glassine_installer::plan::{Input, plan};
use glassine_installer::recognize::recognize;
use glassine_installer::record::{Backup, Confirmation, Record, Result_, StepState};
use glassine_installer::table::{ReadAt, crc32};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::pin::pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

const SECTOR: u64 = 512;
const DISK_SECTORS: u64 = 61_071_360;
const MIB: usize = 1 << 20;

fn block<F: Future>(f: F) -> F::Output {
    pollster::block_on(f)
}

/// Polls a future the simulations finish at once.
fn now<F: Future>(f: F) -> F::Output {
    let mut f = pin!(f);
    match f.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("a simulated transfer waited"),
    }
}

// ---- the device --------------------------------------------------------------------

/// The stock primary table with vendor at 8 MiB.
fn stock_table() -> Vec<u8> {
    let mut b = std::fs::read(common::root().join("core/tests/fixtures/gpt/noteair-stock.gpt")).unwrap();
    let e = 1024 + 10 * 128; // slot 11: vendor
    let first = u64::from_le_bytes(b[e + 32..e + 40].try_into().unwrap());
    b[e + 40..e + 48].copy_from_slice(&(first + 16383).to_le_bytes());
    let crc = crc32(&b[1024..1024 + 48 * 128]);
    b[512 + 88..512 + 92].copy_from_slice(&crc.to_le_bytes());
    b[512 + 16..512 + 20].fill(0);
    let hc = crc32(&b[512..512 + 92]);
    b[512 + 16..512 + 20].copy_from_slice(&hc.to_le_bytes());
    b
}

struct FbT(Rc<RefCell<FbSim>>);
impl Transport for FbT {
    async fn send(&mut self, d: &[u8]) -> Result<(), FbError> {
        now(self.0.borrow_mut().send(d))
    }
    async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
        now(self.0.borrow_mut().recv())
    }
}

struct RamT(Rc<RefCell<RamEnv>>);
impl Transport for RamT {
    async fn send(&mut self, d: &[u8]) -> Result<(), FbError> {
        now(self.0.borrow_mut().send(d))
    }
    async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
        now(self.0.borrow_mut().recv())
    }
    async fn recv_max(&mut self, max: usize) -> Result<Vec<u8>, FbError> {
        now(self.0.borrow_mut().recv_max(max))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum At {
    Android,
    Fastboot,
    Ram,
    Glassine,
}

/// The artifacts of the test release, by role.
fn release_files() -> BTreeMap<&'static str, (Vec<u8>, Option<&'static str>)> {
    // The layout releases carry (GUIDs left empty: derived from the unit's disk GUID).
    let recipe: serde_json::Value = serde_json::from_slice(&std::fs::read(common::root().join("layouts/onyx-noteair.json")).unwrap()).unwrap();
    BTreeMap::from([
        ("boot-loader", (image(200 * 1024, 31), Some("raw-image"))),
        ("ram-environment", (image(MIB, 32), Some("raw-image"))),
        ("partition-layout", (serde_json::to_vec_pretty(&recipe).unwrap(), Some("gpt-recipe"))),
        ("kernel", (image(MIB + 300, 33), Some("raw-image"))),
        ("base", (image(3 * MIB, 34), Some("btrfs-pool-image"))),
        ("recovery", (image(MIB, 35), Some("raw-image"))),
    ])
}

fn source(files: &BTreeMap<&'static str, (Vec<u8>, Option<&'static str>)>) -> MemorySource {
    let mut m = serde_json::Map::new();
    let mut src = MemorySource {
        origin: "test release".into(),
        files: BTreeMap::new(),
    };
    for (role, (b, kind)) in files {
        let file = format!("{role}.img");
        m.insert(
            role.to_string(),
            serde_json::json!({"file": file, "sha256": sha256_hex(b), "size": b.len(), "kind": kind}),
        );
        src.files.insert(file, b.clone());
    }
    let manifest = serde_json::json!({"release_format": 1, "artifacts": m});
    src.files.insert("release.json".into(), serde_json::to_vec(&manifest).unwrap());
    src
}

struct World {
    at: At,
    abl: Vec<u8>,
    recovery: Vec<u8>,
    devinfo: Vec<u8>,
    ram: Rc<RefCell<RamEnv>>,
    fb: Option<Rc<RefCell<FbSim>>>,
    boot_loader: Vec<u8>,
    ram_environment: Vec<u8>,
    source: Vec<u8>,
    /// A flash of abl never takes: the stock boot loader comes back.
    abl_never_takes: bool,
    erased_whole_disk: bool,
    reaches: usize,
    /// Each fastboot session's commands, in order.
    sessions: Vec<Vec<String>>,
}

impl World {
    fn new() -> World {
        let files = release_files();
        let src = stock_table();
        let mut disk = Sparse::new(DISK_SECTORS * SECTOR);
        disk.write(0, &src);
        // vendor's 8 MiB hold something to copy
        let g = parse_gpt(&src, SECTOR).unwrap();
        let (v, n) = g.extent("vendor").unwrap();
        disk.write(v, &image(n as usize, 40));
        let ram = RamEnv::new(vec![("mmcblk0".into(), disk), ("mmcblk0boot0".into(), Sparse::new(4 * MIB as u64))]);
        World {
            at: At::Android,
            abl: image(MIB, 41),
            recovery: image(8 * MIB, 42),
            devinfo: image(4096, 43),
            ram: Rc::new(RefCell::new(ram)),
            fb: None,
            boot_loader: files["boot-loader"].0.clone(),
            ram_environment: files["ram-environment"].0.clone(),
            source: src,
            abl_never_takes: false,
            erased_whole_disk: false,
            reaches: 0,
            sessions: vec![],
        }
    }

    /// The stock boot partition (slot A's, after the table step) holding `img`.
    fn with_boot(self, img: &[u8]) -> World {
        let (b, _) = parse_gpt(&self.source, SECTOR).unwrap().extent("boot").unwrap();
        self.ram.borrow_mut().disks[0].1.write(b, img);
        self
    }
    fn glassine_boot_loader(&self) -> bool {
        self.abl.starts_with(&self.boot_loader)
    }

    /// What the last session left: partitions written, where it restarted to.
    fn sync(&mut self) {
        if let Some(fb) = self.fb.take() {
            let s = fb.borrow();
            self.sessions.push(s.commands.clone());
            if !self.abl_never_takes {
                self.abl = s.parts["abl"].clone();
            }
            self.recovery = s.parts["recovery"].clone();
            self.devinfo = s.parts["devinfo"].clone();
            self.erased_whole_disk |= s.erased_whole_disk;
            match s.rebooted.last().map(String::as_str) {
                Some("bootloader") => self.at = At::Fastboot,
                Some("reboot-recovery") => {
                    if self.recovery.starts_with(&self.ram_environment) {
                        self.at = At::Ram;
                        self.ram.borrow_mut().restart();
                    } else {
                        self.at = At::Android;
                    }
                }
                Some("reboot") => self.at = if self.glassine_boot_loader() { At::Glassine } else { At::Android },
                _ => {}
            }
        }
        let r = self.ram.borrow().rebooted;
        if let Some(i) = r {
            self.at = if i == 1 { At::Fastboot } else { At::Glassine };
            self.ram.borrow_mut().rebooted = None;
        }
    }

    fn link(&mut self) -> Link<FbT, RamT> {
        match self.at {
            At::Fastboot => {
                let parts: Vec<(&str, Vec<u8>)> = vec![
                    ("abl", self.abl.clone()),
                    ("recovery", self.recovery.clone()),
                    ("devinfo", self.devinfo.clone()),
                ];
                let sim = if self.glassine_boot_loader() {
                    FbSim::glassine_noteair(&parts)
                } else {
                    FbSim::stock_noteair(&parts)
                };
                let rc = Rc::new(RefCell::new(sim));
                self.fb = Some(rc.clone());
                Link::Fastboot(FbT(rc))
            }
            At::Ram => Link::Ram(RamT(self.ram.clone())),
            At::Android => Link::Elsewhere("android".into()),
            At::Glassine => Link::Elsewhere("glassine".into()),
        }
    }
}

struct Edge {
    w: World,
    saved: Option<String>,
    saves: usize,
    cut_at_save: Option<usize>,
    cancel_after: Option<String>,
    last_done: Vec<String>,
    notes: Vec<String>,
    asked: Vec<String>,
    /// What the executor kept on the computer (a boot image's original).
    kept: BTreeMap<String, Vec<u8>>,
    /// The manifest hash of the backup the edge reads (None: no backup given).
    backup_manifest: Option<String>,
}

impl Edge {
    fn new(w: World) -> Edge {
        Edge {
            w,
            saved: None,
            saves: 0,
            cut_at_save: None,
            cancel_after: None,
            last_done: vec![],
            notes: vec![],
            asked: vec![],
            kept: BTreeMap::new(),
            backup_manifest: Some(sha256_hex(b"manifest")),
        }
    }
}

impl InstallEdge for Edge {
    type Fb = FbT;
    type Ram = RamT;

    async fn reach(&mut self, mode: &Mode, how: Reach) -> Result<Link<FbT, RamT>, String> {
        self.w.reaches += 1;
        self.w.sync();
        match how {
            Reach::Already { .. } => {
                // after a cut: a RAM environment that went away was restarted by
                // the person into the boot loader (cover closed)
                if self.w.at == At::Ram && self.w.ram.borrow().gone {
                    self.w.at = At::Fastboot;
                }
            }
            Reach::Sent { .. } => {}
            Reach::Perform { transition, .. } => match transition.as_str() {
                "android-to-fastboot-stock" if self.w.at == At::Android => self.w.at = At::Fastboot,
                t => return Err(format!("the test edge can't perform {t} from {:?}", self.w.at)),
            },
            Reach::AskPerson { transition, text } => {
                self.asked.push(text);
                match transition.as_deref() {
                    Some("fastboot-by-hand") => self.w.at = At::Fastboot,
                    t => return Err(format!("the person can't do {t:?}")),
                }
            }
        }
        let _ = mode;
        Ok(self.w.link())
    }

    async fn save(&mut self, record: &Record) -> Result<(), String> {
        self.saves += 1;
        if self.cut_at_save == Some(self.saves) {
            return Err("the computer lost power".into());
        }
        self.last_done = record.done().unwrap();
        self.saved = Some(record.to_json());
        Ok(())
    }

    async fn back_up(&mut self, _step: &Step) -> Result<Backup, String> {
        self.w.at = At::Android;
        Ok(Backup {
            location: "the test's backup".into(),
            manifest_sha256: self.backup_manifest.clone(),
        })
    }

    async fn backup_manifest(&mut self) -> Result<Option<String>, String> {
        Ok(self.backup_manifest.clone())
    }

    async fn backup_bytes(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, String> {
        let mut b = vec![0u8; len];
        let mut s: &[u8] = &self.w.source;
        s.read_at(offset, &mut b)?;
        Ok(b)
    }

    async fn keep(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
        self.kept.insert(name.into(), bytes.to_vec());
        Ok(())
    }

    async fn kept(&mut self, name: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self.kept.get(name).cloned())
    }

    fn progress(&mut self, _: &str, _: u64, _: u64) {}

    fn note(&mut self, line: &str) {
        self.notes.push(line.into());
    }

    fn cancel_requested(&mut self) -> bool {
        self.cancel_after.as_ref().is_some_and(|s| self.last_done.contains(s))
    }
}

// ---- the job ---------------------------------------------------------------------

fn description() -> Description {
    valid(&noteair())
}

/// The developer plan without stock Android (every write step of the job).
fn the_plan(d: &Description, developer: bool) -> String {
    the_plan_keeping(d, developer, false)
}

/// The plan with Android kept on slot A (`keep`) or retired.
fn the_plan_keeping(d: &Description, developer: bool, keep: bool) -> String {
    let ds = [d];
    let f = facts("noteair-stock-fastboot");
    let rec = recognize(&ds, &f);
    let input = Input {
        choices: BTreeMap::from([("keep-stock".to_string(), if keep { "yes" } else { "no" }.to_string())]),
        hardware: BTreeMap::from([("emergency-cable".to_string(), true)]),
        developer,
        ..Default::default()
    };
    let p = plan(&ds, &f, &rec, &input);
    serde_json::to_string_pretty(&p).unwrap()
}

fn new_record(d: &Description, plan: &str, src: MemorySource) -> Record {
    let a = block(Artifacts::open(src)).unwrap();
    let arts = a.roles().iter().map(|r| a.artifact(r).unwrap()).collect();
    Record::new(
        "test",
        &d.id,
        noteair().as_bytes(),
        plan,
        Confirmation {
            text: "install".into(),
            at: None,
        },
        arts,
        None,
    )
    .unwrap()
}

fn job(d: &Description) -> Job<'_> {
    Job::new(d, sha256_hex(noteair().as_bytes()), true, "install").unwrap()
}

fn go(d: &Description, plan: &str, record: &mut Record, src: MemorySource, edge: &mut Edge) -> Result<Outcome, Stopped> {
    let mut a = block(Artifacts::open(src)).unwrap();
    block(run(&job(d), plan, record, &mut a, edge))
}

/// The device as an install leaves it.
fn installed(w: &World) {
    installed_as(w, false)
}

/// The device as an install leaves it, Android kept on slot A (`keep`) or retired.
fn installed_as(w: &World, keep: bool) {
    let files = release_files();
    assert!(w.glassine_boot_loader(), "abl holds the GlassineOS boot loader");
    assert!(w.recovery.starts_with(&w.ram_environment));
    assert!(!w.erased_whole_disk);
    let ram = w.ram.borrow();
    let disk = &ram.disks[0].1;
    assert!(ram.armed.is_none(), "every disk read-only again");
    let primary = disk.read(0, 18 * SECTOR);
    let g = parse_gpt(&primary, SECTOR).unwrap();
    assert_eq!(g.entries, 64);
    let names: Vec<&str> = g.parts.iter().map(|p| p.name.as_str()).collect();
    for n in ["boot_a", "system_a", "vendor_a", "boot_b", "system_b", "vendor_b", "userdata", "recovery"] {
        assert!(names.contains(&n), "{n} in {names:?}");
    }
    let attrs = |n: &str| g.part(n).unwrap().attributes >> 48 & 0xff;
    assert_eq!(attrs("boot_b"), 0x7f, "slot B pinned");
    if keep {
        assert_eq!(attrs("boot_a"), 0x7d, "slot A a bootable fallback");
    } else {
        assert_eq!(attrs("boot_a"), 0x80, "slot A retired");
    }
    assert_eq!(g.part("vendor_b").unwrap().attributes >> 56, 0x10, "bits above the slot flags kept");
    // the backup copy holds the same table
    let mut whole = DiskRead(disk);
    let mut t = glassine_installer::table::read(&mut whole, DISK_SECTORS * SECTOR, SECTOR).unwrap();
    assert!(t.primary_ok);
    let mut bad = TornPrimary(disk);
    let b = glassine_installer::table::read(&mut bad, DISK_SECTORS * SECTOR, SECTOR).unwrap();
    assert!(!b.primary_ok, "read from the backup copy");
    t.primary_ok = false;
    assert_eq!(format!("{:?}", t.entries), format!("{:?}", b.entries));
    // slot B
    let at = |n: &str| g.extent(n).unwrap();
    let k = &files["kernel"].0;
    let (o, _) = at("boot_b");
    let mut padded = k.clone();
    padded.resize(k.len().div_ceil(512) * 512, 0);
    assert_eq!(disk.read(o, padded.len() as u64), padded);
    let (o, _) = at("system_b");
    assert_eq!(disk.read(o, files["base"].0.len() as u64), files["base"].0);
    let (va, n) = at("vendor_a");
    let (vb, m) = at("vendor_b");
    assert_eq!(n, m);
    assert_eq!(disk.read(va, n), disk.read(vb, n));
    assert_eq!(disk.read(va, n), image(n as usize, 40));
    // the GlassineOS recovery isn't built: the developer's plan leaves its
    // step out and the recovery partition keeps the RAM environment
    // nothing but the table and the steps' partitions was written
    for (_, off, len) in &ram.written {
        let inside = ["boot_b", "system_b", "vendor_b", "recovery", if keep { "boot_a" } else { "boot_b" }]
            .iter()
            .any(|n| {
                let (o, l) = at(n);
                *off >= o && off + len <= o + l
            })
            || off + len <= 18 * SECTOR
            || *off >= (DISK_SECTORS - 17) * SECTOR;
        assert!(inside, "a write at {off}+{len}");
    }
}

struct DiskRead<'a>(&'a Sparse);
impl ReadAt for DiskRead<'_> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        buf.copy_from_slice(&self.0.read(offset, buf.len() as u64));
        Ok(())
    }
}

/// The disk with its primary header broken.
struct TornPrimary<'a>(&'a Sparse);
impl ReadAt for TornPrimary<'_> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        buf.copy_from_slice(&self.0.read(offset, buf.len() as u64));
        if offset <= 512 + 20 && offset + buf.len() as u64 > 512 + 20 {
            buf[(512 + 20 - offset) as usize] ^= 1;
        }
        Ok(())
    }
}

/// A run that keeps Android, its boot partition holding `boot`.
fn fresh_keeping(boot: &[u8]) -> (Description, String, MemorySource, Record, Edge) {
    let d = description();
    let plan = the_plan_keeping(&d, true, true);
    let src = source(&release_files());
    let rec = new_record(&d, &plan, src.clone());
    (d, plan, src, rec, Edge::new(World::new().with_boot(boot)))
}

/// Slot A's boot partition as the device holds it, `n` bytes.
fn boot_a(w: &World, n: usize) -> Vec<u8> {
    let ram = w.ram.borrow();
    let disk = &ram.disks[0].1;
    let g = parse_gpt(&disk.read(0, 18 * SECTOR), SECTOR).unwrap();
    disk.read(g.extent("boot_a").unwrap().0, n as u64)
}

fn fresh() -> (Description, String, MemorySource, Record, Edge) {
    let d = description();
    let plan = the_plan(&d, true);
    let src = source(&release_files());
    let rec = new_record(&d, &plan, src.clone());
    (d, plan, src, rec, Edge::new(World::new()))
}

// ---- tests ---------------------------------------------------------------------------

#[test]
fn the_whole_job_installs_and_waits_for_the_persons_check() {
    let (d, plan, src, mut rec, mut edge) = fresh();
    let r = go(&d, &plan, &mut rec, src, &mut edge);
    match r {
        Ok(Outcome::WaitingForAcceptance { mode, check }) => {
            assert_eq!(mode, "glassine");
            assert_eq!(check, "Start the device normally.");
        }
        r => panic!("{r:?}\n{:#?}", edge.notes),
    }
    edge.w.sync();
    installed(&edge.w);
    assert_eq!(edge.w.at, At::Glassine);
    for s in rec.plan_steps().unwrap() {
        assert_eq!(rec.state(&s), StepState::Done, "{s}");
    }
    // the saved record is the record
    assert_eq!(Record::parse(edge.saved.as_ref().unwrap()).unwrap(), rec);
    // every intent was recorded with the artifact's hash before its outcome
    let k = rec.journal.iter().find(|e| e.step == "write-kernel").unwrap();
    assert!(k.intent.summary.contains(&rec.artifacts.iter().find(|a| a.role == "kernel").unwrap().sha256));
    let ev = &k.outcome.as_ref().unwrap().evidence;
    assert!(ev["read-back"].contains("match"), "{ev:?}");
    assert!(edge.notes.iter().any(|n| n.contains("armed")));
    // the RAM environment was entered by oem reboot-recovery in the same
    // session as the RAM environment's flash, nothing between them
    let s = edge
        .w
        .sessions
        .iter()
        .find(|c| c.iter().any(|x| x == "oem reboot-recovery"))
        .expect("an oem reboot-recovery");
    let flash = s.iter().position(|x| x == "flash:recovery").expect("the flash in that session");
    let oem = s.iter().position(|x| x == "oem reboot-recovery").unwrap();
    assert_eq!(oem, flash + 1, "{s:?}");
}

#[test]
fn a_cut_at_every_save_resumes_to_the_same_device() {
    let mut k = 1;
    let (mut rearmed, mut boot_loader_found) = (0, 0);
    loop {
        let (d, plan, src, mut rec, mut edge) = fresh();
        let first = rec.clone();
        edge.cut_at_save = Some(k);
        let r = go(&d, &plan, &mut rec, src.clone(), &mut edge);
        let Err(stopped) = r else {
            assert!(k > 20, "the run saves more than 20 times");
            break;
        };
        assert!(stopped.escape.is_some() || stopped.step.is_none(), "{stopped:?}");
        // resume from what was saved
        let mut rec = match &edge.saved {
            Some(t) => Record::parse(t).unwrap(),
            None => first,
        };
        edge.cut_at_save = None;
        let r = go(&d, &plan, &mut rec, src, &mut edge);
        assert!(
            matches!(r, Ok(Outcome::WaitingForAcceptance { .. })),
            "cut at save {k} ({:?}): {r:?}\n{:#?}",
            stopped,
            edge.notes
        );
        installed(&edge.w);
        rearmed += edge.notes.iter().filter(|n| n.contains("armed by an earlier run")).count();
        boot_loader_found += rec
            .journal
            .iter()
            .filter(|e| e.step == "replace-boot-loader" && e.outcome.as_ref().is_some_and(|o| o.evidence.contains_key("found-done")))
            .count();
        k += 1;
    }
    eprintln!("{k} cuts, {rearmed} RAM environments restarted, {boot_loader_found} boot loaders found done");
    assert!(rearmed > 0, "a cut while armed restarts the RAM environment");
    assert!(boot_loader_found > 0, "a cut boot-loader step is decided by the boot loader that answers");
}

#[test]
fn the_final_restart_is_recorded_and_a_resume_after_it_leaves_the_device_alone() {
    let (d, plan, src, mut rec, mut edge) = fresh();
    let r = go(&d, &plan, &mut rec, src.clone(), &mut edge);
    assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "{r:?}");
    let f = rec.final_restart.clone().expect("the restart is in the record");
    assert!(f.sent);
    assert_eq!(f.mode, "glassine");
    // resume: no reach, no write, the same check
    let mut rec = Record::parse(edge.saved.as_ref().unwrap()).unwrap();
    let (reaches, writes) = (edge.w.reaches, edge.w.ram.borrow().written.len());
    let r = go(&d, &plan, &mut rec, src, &mut edge);
    match r {
        Ok(Outcome::WaitingForAcceptance { mode, check }) => {
            assert_eq!(mode, f.mode);
            assert_eq!(check, f.check);
        }
        r => panic!("{r:?}"),
    }
    assert_eq!(edge.w.reaches, reaches, "the device isn't looked for");
    assert_eq!(edge.w.ram.borrow().written.len(), writes);
}

#[test]
fn a_cut_just_before_or_after_the_final_restart_resumes_to_the_check() {
    // the whole run's saves: the last two are the restart's (before, after)
    let (d, plan, src, mut rec, mut edge) = fresh();
    go(&d, &plan, &mut rec, src.clone(), &mut edge).unwrap();
    let n = edge.saves;
    for (cut, sent) in [(n - 1, false), (n, true)] {
        let (d, plan, src, mut rec, mut edge) = fresh();
        edge.cut_at_save = Some(cut);
        assert!(go(&d, &plan, &mut rec, src.clone(), &mut edge).is_err());
        edge.w.sync();
        // before: the device still waits in the RAM environment; after: it restarted
        assert_eq!(edge.w.at == At::Glassine, sent, "cut at save {cut}");
        let mut rec = Record::parse(edge.saved.as_ref().unwrap()).unwrap();
        assert_eq!(rec.final_restart.is_some(), sent, "the record of a cut at save {cut}");
        assert!(rec.final_restart.as_ref().is_none_or(|f| !f.sent));
        edge.cut_at_save = None;
        let r = go(&d, &plan, &mut rec, src, &mut edge);
        assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "cut at save {cut}: {r:?}");
        edge.w.sync();
        assert_eq!(edge.w.at, At::Glassine, "cut at save {cut}: restarted once");
        assert!(rec.final_restart.as_ref().unwrap().sent);
        installed(&edge.w);
    }
}

#[test]
fn a_cut_inside_a_slot_flag_write_resumes_from_the_whole_copy() {
    // the run's last WRITEs are the slot steps' table copies (backup, then primary)
    let (d, plan, src, mut rec, mut edge) = fresh();
    go(&d, &plan, &mut rec, src.clone(), &mut edge).unwrap();
    let total = edge.w.ram.borrow().writes;
    let slot_steps: Vec<_> = d.steps.iter().filter(|s| s.kind == "set-boot-slot").map(|s| s.id.clone()).collect();
    assert!(!slot_steps.is_empty());
    let mut rebuilt = 0;
    for back in 1..=2 * slot_steps.len() {
        let (d, plan, src, mut rec, mut edge) = fresh();
        let n = total - back;
        edge.w.ram.borrow_mut().faults = WriteFaults {
            cut_write: Some(n),
            ..Default::default()
        };
        let s = go(&d, &plan, &mut rec, src.clone(), &mut edge).unwrap_err();
        assert!(slot_steps.contains(&s.step.clone().unwrap()), "WRITE {n}: {s:?}");
        assert!(s.may_have_changed);
        edge.w.ram.borrow_mut().faults = WriteFaults::default();
        let mut rec = Record::parse(edge.saved.as_ref().unwrap()).unwrap();
        let r = go(&d, &plan, &mut rec, src, &mut edge);
        assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "WRITE {n}: {r:?}\n{:#?}", edge.notes);
        edge.w.sync();
        installed(&edge.w);
        rebuilt += rec
            .journal
            .iter()
            .filter_map(|e| e.outcome.as_ref())
            .filter(|o| o.evidence.get("found").is_some_and(|f| f.contains("rebuilt from the backup copy")))
            .count();
    }
    // a half-written primary whose unwritten half holds no change reads whole
    assert!(rebuilt >= 1, "a torn primary was rebuilt from the backup copy");
}

#[test]
fn a_kept_android_gets_its_boot_image_changed_in_place() {
    let img = boot_image(b"wait", 5000);
    let (d, plan, src, mut rec, mut edge) = fresh_keeping(&img);
    assert!(plan.contains("patch-stock-boot") && !plan.contains("\"retire-stock\""), "{plan}");
    let r = go(&d, &plan, &mut rec, src, &mut edge);
    assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "{r:?}\n{:#?}", edge.notes);
    edge.w.sync();
    installed_as(&edge.w, true);
    let Patched::Changed(want) = patch_vendor_slotselect(&img).unwrap() else {
        panic!()
    };
    assert_eq!(boot_a(&edge.w, want.len()), want);
    assert_eq!(rec.state("patch-stock-boot"), StepState::Done);
    let ev = &rec
        .journal
        .iter()
        .find(|e| e.step == "patch-stock-boot")
        .unwrap()
        .outcome
        .as_ref()
        .unwrap()
        .evidence;
    assert_eq!(ev["original-sha256"].len(), 64);
    assert!(ev["written"].starts_with(&format!("{} of", 16 * MIB)), "{ev:?}");
    // the original was kept on the computer before the first write
    assert!(edge.kept.values().any(|b| b.starts_with(&img)));
}

#[test]
fn a_boot_image_the_change_refuses_is_left_and_the_run_goes_on() {
    // the stock image's vendor entry still checks the partition
    let img = boot_image(b"wait,verify", 5000);
    let (d, plan, src, mut rec, mut edge) = fresh_keeping(&img);
    let r = go(&d, &plan, &mut rec, src.clone(), &mut edge);
    assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "{r:?}");
    edge.w.sync();
    installed_as(&edge.w, true);
    assert_eq!(boot_a(&edge.w, img.len()), img, "nothing written");
    assert_eq!(rec.state("patch-stock-boot"), StepState::Failed);
    let o = rec
        .journal
        .iter()
        .rev()
        .find(|e| e.step == "patch-stock-boot")
        .unwrap()
        .outcome
        .clone()
        .unwrap();
    assert!(o.message.as_deref().unwrap().contains("Nothing was written"), "{o:?}");
    assert_eq!(rec.state("set-slot"), StepState::Done, "the run went on to the slot");
    // a nothing-to-do image is done without a write
    let Patched::Changed(done) = patch_vendor_slotselect(&boot_image(b"wait", 100)).unwrap() else {
        panic!()
    };
    let (d, plan, src, mut rec, mut edge) = fresh_keeping(&done);
    go(&d, &plan, &mut rec, src, &mut edge).unwrap();
    let e = rec.journal.iter().find(|e| e.step == "patch-stock-boot").unwrap();
    assert!(e.outcome.as_ref().unwrap().evidence["found"].contains("already"));
}

#[test]
fn a_refused_boot_image_doesnt_hold_a_resume_after_the_final_restart() {
    let img = boot_image(b"wait,verify", 5000);
    let (d, plan, src, mut rec, mut edge) = fresh_keeping(&img);
    go(&d, &plan, &mut rec, src.clone(), &mut edge).unwrap();
    let n = edge.saves;
    // resumed after the run ended: the check, the device left alone
    let mut again = Record::parse(edge.saved.as_ref().unwrap()).unwrap();
    let reaches = edge.w.reaches;
    let r = go(&d, &plan, &mut again, src.clone(), &mut edge);
    assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "{r:?}");
    assert_eq!(edge.w.reaches, reaches);
    // cut at the restart's two saves (before it is sent, after)
    for cut in [n - 1, n] {
        let (d, plan, src, mut rec, mut edge) = fresh_keeping(&img);
        edge.cut_at_save = Some(cut);
        assert!(go(&d, &plan, &mut rec, src.clone(), &mut edge).is_err());
        edge.cut_at_save = None;
        let mut rec = Record::parse(edge.saved.as_ref().unwrap()).unwrap();
        let r = go(&d, &plan, &mut rec, src, &mut edge);
        assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "cut at save {cut}: {r:?}");
        edge.w.sync();
        assert_eq!(edge.w.at, At::Glassine);
        installed_as(&edge.w, true);
        assert_eq!(rec.state("patch-stock-boot"), StepState::Failed);
    }
}

#[test]
fn a_cut_inside_the_boot_image_write_resumes_from_the_kept_original() {
    // a kernel past half of the first WRITE: the cut leaves the new header
    // and the old device tree
    let img = boot_image(b"wait", 9 * MIB);
    let (d, plan, src, mut rec, mut edge) = fresh_keeping(&img);
    go(&d, &plan, &mut rec, src.clone(), &mut edge).unwrap();
    let boot_at = {
        let ram = edge.w.ram.borrow();
        let g = parse_gpt(&ram.disks[0].1.read(0, 18 * SECTOR), SECTOR).unwrap();
        let (o, _) = g.extent("boot_a").unwrap();
        ram.written.iter().position(|(_, off, _)| *off == o).expect("boot_a written")
    };
    let (d, plan, src, mut rec, mut edge) = fresh_keeping(&img);
    edge.w.ram.borrow_mut().faults = WriteFaults {
        cut_write: Some(boot_at),
        ..Default::default()
    };
    let s = go(&d, &plan, &mut rec, src.clone(), &mut edge).unwrap_err();
    assert_eq!(s.step.as_deref(), Some("patch-stock-boot"), "{s:?}");
    assert!(s.may_have_changed);
    let torn = boot_a(&edge.w, img.len());
    assert_ne!(torn, img);
    assert!(
        patch_vendor_slotselect(&torn).is_err(),
        "the half-written image is refused: only the original can be changed again"
    );
    edge.w.ram.borrow_mut().faults = WriteFaults::default();
    let mut rec = Record::parse(edge.saved.as_ref().unwrap()).unwrap();
    let r = go(&d, &plan, &mut rec, src, &mut edge);
    assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "{r:?}\n{:#?}", edge.notes);
    edge.w.sync();
    installed_as(&edge.w, true);
    let Patched::Changed(want) = patch_vendor_slotselect(&img).unwrap() else {
        panic!()
    };
    assert_eq!(boot_a(&edge.w, want.len()), want);
    assert!(edge.notes.iter().any(|n| n.contains("as read before the first write")));
}

#[test]
fn a_cut_between_the_table_copies_leaves_a_whole_copy_and_resumes() {
    let (d, plan, src, mut rec, mut edge) = fresh();
    // the RAM environment's writes: 0 = the backup copy, 1 = the primary
    edge.w.ram.borrow_mut().faults = WriteFaults {
        cut_write: Some(1),
        ..Default::default()
    };
    let s = go(&d, &plan, &mut rec, src.clone(), &mut edge).unwrap_err();
    assert_eq!(s.step.as_deref(), Some("partition-table"));
    assert!(s.may_have_changed);
    {
        let ram = edge.w.ram.borrow();
        let disk = &ram.disks[0].1;
        let t = glassine_installer::table::read(&mut DiskRead(disk), DISK_SECTORS * SECTOR, SECTOR).unwrap();
        assert!(!t.primary_ok, "the torn primary is refused");
        assert!(t.entries.iter().any(|e| e.name.as_deref() == Some("boot_b")), "the new backup copy is whole");
    }
    edge.w.ram.borrow_mut().faults = WriteFaults::default();
    let mut rec = Record::parse(edge.saved.as_ref().unwrap()).unwrap();
    assert_eq!(rec.open_step(), None, "the failure was recorded");
    assert_eq!(rec.state("partition-table"), StepState::Failed);
    let r = go(&d, &plan, &mut rec, src, &mut edge);
    assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "{r:?}");
    installed(&edge.w);
}

#[test]
fn a_read_back_that_differs_stops_the_step_and_a_retry_finishes() {
    let (d, plan, src, mut rec, mut edge) = fresh();
    // the 3rd WRITE of the start: the first of the kernel's
    edge.w.ram.borrow_mut().faults = WriteFaults {
        flip_write: Some(2),
        ..Default::default()
    };
    let s = go(&d, &plan, &mut rec, src.clone(), &mut edge).unwrap_err();
    assert_eq!(s.step.as_deref(), Some("write-kernel"), "{s}");
    assert!(s.may_have_changed && s.why.contains("read back differently"), "{s}");
    assert_eq!(s.escape.as_ref().unwrap().way_out, "step write-kernel");
    assert!(edge.w.ram.borrow().armed.is_none(), "a stopped run leaves the disks read-only");
    edge.w.ram.borrow_mut().faults = WriteFaults::default();
    let r = go(&d, &plan, &mut rec, src, &mut edge);
    assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "{r:?}");
    installed(&edge.w);
    let tries: Vec<Result_> = rec
        .journal
        .iter()
        .filter(|e| e.step == "write-kernel")
        .map(|e| e.outcome.as_ref().unwrap().result)
        .collect();
    assert_eq!(tries, [Result_::Failed, Result_::Done]);
}

#[test]
fn a_ram_environment_gone_mid_write_is_restarted_through_the_boot_loader() {
    let (d, plan, src, mut rec, mut edge) = fresh();
    edge.w.ram.borrow_mut().faults = WriteFaults {
        cut_write: Some(4),
        ..Default::default()
    };
    let s = go(&d, &plan, &mut rec, src.clone(), &mut edge).unwrap_err();
    assert!(s.may_have_changed, "{s}");
    edge.w.ram.borrow_mut().faults = WriteFaults::default();
    let mut rec = Record::parse(edge.saved.as_ref().unwrap()).unwrap();
    let r = go(&d, &plan, &mut rec, src, &mut edge);
    assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "{r:?}\n{:#?}", edge.notes);
    installed(&edge.w);
}

#[test]
fn a_boot_loader_that_comes_back_as_the_old_one_stops_with_the_cable_escape() {
    let (d, plan, src, mut rec, mut edge) = fresh();
    edge.w.abl_never_takes = true;
    let s = go(&d, &plan, &mut rec, src, &mut edge).unwrap_err();
    assert_eq!(s.step.as_deref(), Some("replace-boot-loader"));
    let e = s.escape.unwrap();
    assert_eq!(e.hardware.as_deref(), Some("emergency-cable"));
    assert_eq!(e.way_out, "transition cable-to-emergency");
    assert_eq!(rec.state("replace-boot-loader"), StepState::Failed);
    assert_eq!(rec.state("write-ram-environment"), StepState::NotStarted);
}

#[test]
fn a_changed_artifact_stops_before_the_device_is_touched() {
    let (d, plan, _, mut rec, mut edge) = fresh();
    let mut files = release_files();
    files.get_mut("kernel").unwrap().0[5] ^= 1;
    let s = go(&d, &plan, &mut rec, source(&files), &mut edge).unwrap_err();
    assert!(s.why.contains("kernel") && s.why.contains("not the confirmed artifact"), "{s}");
    assert_eq!(edge.w.reaches, 0);
    assert!(rec.journal.is_empty());
    // a file that isn't what its manifest says
    let (d, plan, mut src, mut rec, mut edge) = fresh();
    src.files.get_mut("base.img").unwrap()[0] ^= 1;
    let s = go(&d, &plan, &mut rec, src, &mut edge).unwrap_err();
    assert!(s.why.contains("SHA-256"), "{s}");
    assert_eq!(edge.w.reaches, 0);
}

/// A run stopped after write-kernel (partition-table done, RAM steps left):
/// the resume's state before anything is tried.
fn stopped_after_the_table() -> (Description, String, MemorySource, Record, Edge) {
    let (d, plan, src, mut rec, mut edge) = fresh();
    edge.cancel_after = Some("write-kernel".into());
    go(&d, &plan, &mut rec, src.clone(), &mut edge).unwrap_err();
    assert_eq!(rec.state("partition-table"), StepState::Done);
    assert_eq!(rec.state("write-base"), StepState::NotStarted);
    edge.cancel_after = None;
    (d, plan, src, rec, edge)
}

/// A resume `edge` refused before it looked for the device or wrote.
fn refused_untouched(s: &Stopped, rec: &Record, journal: usize, edge: &Edge, reaches: usize, writes: usize) {
    assert!(s.step.is_none() && !s.may_have_changed, "{s:?}");
    assert_eq!(edge.w.reaches, reaches, "the device isn't looked for");
    assert_eq!(edge.w.ram.borrow().written.len(), writes, "nothing is written");
    assert_eq!(rec.journal.len(), journal, "no intent is recorded");
}

#[test]
fn a_resume_with_another_layout_is_refused_after_its_step_is_done() {
    let (d, plan, _, mut rec, mut edge) = stopped_after_the_table();
    let (reaches, writes, journal) = (edge.w.reaches, edge.w.ram.borrow().written.len(), rec.journal.len());
    // another release folder: the same recipe written another way, its
    // manifest true to its files, so only the record tells them apart
    let mut files = release_files();
    let recipe: serde_json::Value = serde_json::from_slice(&files["partition-layout"].0).unwrap();
    files.get_mut("partition-layout").unwrap().0 = serde_json::to_vec(&recipe).unwrap();
    let s = go(&d, &plan, &mut rec, source(&files), &mut edge).unwrap_err();
    assert!(s.why.contains("partition-layout") && s.why.contains("not the confirmed artifact"), "{s}");
    refused_untouched(&s, &rec, journal, &edge, reaches, writes);
    // the confirmed release resumes
    let r = go(&d, &plan, &mut rec, source(&release_files()), &mut edge);
    assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "{r:?}");
    installed(&edge.w);
}

#[test]
fn a_resume_with_another_backup_is_refused() {
    let (d, plan, src, mut rec, mut edge) = stopped_after_the_table();
    let (reaches, writes, journal) = (edge.w.reaches, edge.w.ram.borrow().written.len(), rec.journal.len());
    assert_eq!(rec.backup.as_ref().unwrap().manifest_sha256, Some(sha256_hex(b"manifest")));
    edge.backup_manifest = Some(sha256_hex(b"another backup's manifest"));
    let s = go(&d, &plan, &mut rec, src.clone(), &mut edge).unwrap_err();
    assert!(s.why.contains("not the backup this installation rests on"), "{s}");
    refused_untouched(&s, &rec, journal, &edge, reaches, writes);
    // none given, with steps left that read it
    edge.backup_manifest = None;
    let s = go(&d, &plan, &mut rec, src.clone(), &mut edge).unwrap_err();
    assert!(s.why.contains("none is given"), "{s}");
    refused_untouched(&s, &rec, journal, &edge, reaches, writes);
    // the record's backup resumes
    edge.backup_manifest = Some(sha256_hex(b"manifest"));
    let r = go(&d, &plan, &mut rec, src, &mut edge);
    assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "{r:?}");
    installed(&edge.w);
}

#[test]
fn another_plan_or_description_is_refused() {
    let (d, plan, src, mut rec, mut edge) = fresh();
    let other = plan.replacen("\"ready\"", "\"ready\" ", 1);
    let s = go(&d, &other, &mut rec, src.clone(), &mut edge).unwrap_err();
    assert!(s.why.contains("plan"), "{s}");
    let mut j = job(&d);
    j.description_sha256 = sha256_hex(b"another description");
    let mut a = block(Artifacts::open(src)).unwrap();
    let s = block(run(&j, &plan, &mut rec, &mut a, &mut edge)).unwrap_err();
    assert!(s.why.contains("description"), "{s}");
    assert_eq!(edge.w.reaches, 0);
}

#[test]
fn untried_write_steps_need_a_developer() {
    let d = description();
    let src = source(&release_files());
    let mut j = job(&d);
    j.developer = false;
    // a person's plan for this device is refused (its write steps are untried): never run
    let plan = the_plan(&d, false);
    let mut rec = new_record(&d, &plan, src.clone());
    let mut edge = Edge::new(World::new());
    let mut a = block(Artifacts::open(src.clone())).unwrap();
    let s = block(run(&j, &plan, &mut rec, &mut a, &mut edge)).unwrap_err();
    assert!(s.why.contains("nothing is run"), "{s}");
    assert_eq!(edge.w.reaches, 0);
    // a developer's plan, run without asking for untried steps: stopped at the first
    let plan = the_plan(&d, true);
    let mut rec = new_record(&d, &plan, src.clone());
    let mut edge = Edge::new(World::new());
    let mut a = block(Artifacts::open(src)).unwrap();
    let s = block(run(&j, &plan, &mut rec, &mut a, &mut edge)).unwrap_err();
    assert!(s.why.contains("developer"), "{s}");
    assert_eq!(edge.w.reaches, 0);
}

fn with_param(d: &mut Description, step: &str, key: &str, value: &str) {
    let s = d.steps.iter_mut().find(|s| s.id == step).unwrap();
    s.params.insert(key.into(), toml::Value::String(value.into()));
    s.writes = vec![value.into()];
}

#[test]
fn a_never_write_partition_is_refused_before_it_is_sent() {
    let (mut d, plan, src, mut rec, mut edge) = fresh();
    with_param(&mut d, "write-ram-environment", "partition", "devinfo");
    let s = go(&d, &plan, &mut rec, src, &mut edge).unwrap_err();
    assert_eq!(s.step.as_deref(), Some("write-ram-environment"));
    assert!(s.why.contains("never written"), "{s}");
    assert!(!s.may_have_changed);
    edge.w.sync();
    assert_eq!(edge.w.devinfo, image(4096, 43));
}

#[test]
fn the_partition_table_is_never_sent_to_a_boot_loader() {
    let (mut d, plan, src, mut rec, mut edge) = fresh();
    with_param(&mut d, "write-ram-environment", "partition", "partition");
    let s = go(&d, &plan, &mut rec, src, &mut edge).unwrap_err();
    assert!(s.why.contains("erases the whole eMMC"), "{s}");
    let fb = edge.w.fb.as_ref().unwrap().borrow();
    assert!(!fb.sent("flash:partition") && !fb.sent("download:"));
    assert!(!fb.erased_whole_disk);
}

#[test]
fn a_system_partition_is_never_flashed_and_steps_write_only_their_own() {
    // abl switches verified boot to enforcing before a system* flash, which writes devinfo
    let (mut d, plan, src, mut rec, mut edge) = fresh();
    with_param(&mut d, "write-ram-environment", "partition", "system_b");
    let s = go(&d, &plan, &mut rec, src, &mut edge).unwrap_err();
    assert!(s.why.contains("devinfo"), "{s}");
    let fb = edge.w.fb.as_ref().unwrap().borrow();
    assert!(!fb.sent("flash:system_b") && !fb.sent("download:"));
    // a partition no step of the job writes in that mode
    let (mut d, plan, src, mut rec, mut edge) = fresh();
    let st = d.steps.iter_mut().find(|s| s.id == "write-ram-environment").unwrap();
    st.params.insert("partition".into(), toml::Value::String("glassine_scratch".into()));
    let s = go(&d, &plan, &mut rec, src, &mut edge).unwrap_err();
    assert!(s.why.contains("no step of the installation writes glassine_scratch"), "{s}");
}

#[test]
fn the_ram_environment_never_arms_a_never_write_partition() {
    let (mut d, plan, src, mut rec, mut edge) = fresh();
    with_param(&mut d, "write-kernel", "partition", "boot_a");
    let s = go(&d, &plan, &mut rec, src, &mut edge).unwrap_err();
    assert_eq!(s.step.as_deref(), Some("write-kernel"), "{s}");
    let ram = edge.w.ram.borrow();
    assert!(!ram.ops.contains(&5), "nothing was armed");
    assert!(ram.written.is_empty());
}

#[test]
fn a_cancel_is_honoured_after_a_safe_stop_and_the_run_resumes() {
    let (d, plan, src, mut rec, mut edge) = fresh();
    edge.cancel_after = Some("write-kernel".into());
    let s = go(&d, &plan, &mut rec, src.clone(), &mut edge).unwrap_err();
    assert_eq!(s.step.as_deref(), Some("write-kernel"));
    assert!(s.why.contains("cancelled"));
    assert!(edge.w.ram.borrow().armed.is_none());
    assert_eq!(rec.state("write-kernel"), StepState::Done);
    assert_eq!(rec.state("write-base"), StepState::NotStarted);
    edge.cancel_after = None;
    let r = go(&d, &plan, &mut rec, src, &mut edge);
    assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "{r:?}");
    installed(&edge.w);
}

#[test]
fn derived_guids_are_stable_and_per_unit() {
    let a = ops::derived_guid(&[1; 16], "boot_b");
    assert_eq!(a, ops::derived_guid(&[1; 16], "boot_b"));
    assert_ne!(a, ops::derived_guid(&[2; 16], "boot_b"));
    assert_ne!(a, ops::derived_guid(&[1; 16], "system_b"));
    assert_eq!(&a[14..15], "8");
    let _ = CHUNK;
}
