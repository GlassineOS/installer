// SPDX-License-Identifier: GPL-2.0-or-later
//! The backup through the RAM environment, end to end against simulations:
//! a Note Air boot loader in fastboot (equal-length framing, `download` and
//! `boot`) that, once booted, hands over to a simulated RAM environment
//! (tests/common/ramsim.rs), and an edge that keeps the copies in memory.

mod common;
use common::ramsim::{self, Device, Faults};
use common::*;
use glassine_installer::fastboot::{FbError, Transport, readable_variable};
use glassine_installer::format1::Description;
use glassine_installer::glassine_read::Disk;
use glassine_installer::inspect::{Host, UsbIds};
use glassine_installer::ram_backup::*;
use sha2::{Digest, Sha256};
use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

const LEN: usize = 65535;

/// A boot loader that takes equal-length transfers (after the opening and the
/// warm-up's dummy, every transfer is LEN bytes) and answers `getvar`,
/// `download`, its data and `boot`. A transfer of another length after the
/// warm-up is answered FAIL, as the Note Air's garbles it.
struct Boot {
    vars: BTreeMap<String, String>,
    n: usize,
    /// bytes of a download still to come
    expect: usize,
    got: Vec<u8>,
    replies: VecDeque<Vec<u8>>,
    log: Vec<Vec<u8>>,
    booted: Rc<Cell<bool>>,
    /// answers `boot` with FAIL
    refuse_boot: bool,
}

impl Boot {
    fn from_fixture(name: &str, booted: Rc<Cell<bool>>) -> Boot {
        let f = facts(name);
        let vars = f.fastboot.unwrap().variables.into_iter().filter_map(|(k, v)| v.map(|v| (k, v))).collect();
        Boot {
            vars,
            n: 0,
            expect: 0,
            got: vec![],
            replies: VecDeque::new(),
            log: vec![],
            booted,
            refuse_boot: false,
        }
    }

    fn commands(&self) -> Vec<String> {
        // the opening, then every transfer that isn't download data or the dummy
        let mut out = vec![];
        let mut data = 0usize;
        for t in &self.log {
            if data > 0 {
                data -= t.len();
                continue;
            }
            if t.iter().all(|&b| b == b'x') {
                continue;
            }
            let end = t.iter().position(|&b| b == 0).unwrap_or(t.len());
            let c = String::from_utf8_lossy(&t[..end]).into_owned();
            if let Some(h) = c.strip_prefix("download:") {
                data = usize::from_str_radix(h, 16).unwrap();
            }
            out.push(c);
        }
        out
    }
}

impl Transport for Boot {
    async fn send(&mut self, d: &[u8]) -> Result<(), FbError> {
        self.log.push(d.to_vec());
        self.n += 1;
        if self.booted.get() {
            return Err(FbError::Usb("the boot loader is gone".into()));
        }
        if self.n > 1 && d.len() != LEN {
            self.replies.push_back(b"FAILgarbled".to_vec());
            return Ok(());
        }
        if self.expect > 0 {
            self.got.extend_from_slice(d);
            self.expect -= d.len();
            if self.expect == 0 {
                self.replies.push_back(b"OKAY".to_vec());
            }
            return Ok(());
        }
        let end = d.iter().position(|&b| b == 0).unwrap_or(d.len());
        let c = String::from_utf8_lossy(&d[..end]).into_owned();
        let r = if d.iter().all(|&b| b == b'x') {
            b"FAILunknown command".to_vec()
        } else if let Some(v) = c.strip_prefix("getvar:") {
            match self.vars.get(v) {
                Some(x) => format!("OKAY{x}").into_bytes(),
                None => b"FAILunknown variable".to_vec(),
            }
        } else if let Some(h) = c.strip_prefix("download:") {
            let n = usize::from_str_radix(h, 16).unwrap();
            let max: usize = self.vars["max-download-size"].parse().unwrap();
            if n > max {
                b"FAILdata too large".to_vec()
            } else {
                self.expect = n;
                self.got.clear();
                format!("DATA{n:08x}").into_bytes()
            }
        } else if c == "boot" {
            if self.refuse_boot || !self.got.starts_with(b"ANDROID!") {
                b"FAILnot a boot image".to_vec()
            } else {
                self.booted.set(true);
                b"OKAY".to_vec()
            }
        } else {
            b"FAILunknown command".to_vec()
        };
        self.replies.push_back(r);
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
        self.replies.pop_front().ok_or(FbError::Timeout("no answer".into()))
    }
}

/// The simulated RAM environment, still visible to the test after the reader
/// took it.
#[derive(Clone)]
struct Shared(Rc<RefCell<Device>>);

impl Transport for Shared {
    async fn send(&mut self, d: &[u8]) -> Result<(), FbError> {
        self.0.borrow_mut().request(d);
        Ok(())
    }
    async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
        self.recv_max(256).await
    }
    async fn recv_max(&mut self, max: usize) -> Result<Vec<u8>, FbError> {
        let mut d = self.0.borrow_mut();
        pollster::block_on(d.recv_max(max))
    }
}

/// The edge: the RAM environment appears once the boot loader booted; copies
/// are kept in memory.
struct Edge {
    booted: Rc<Cell<bool>>,
    ram: Shared,
    opened: bool,
    ram_ids: UsbIds,
    asked: Option<(RamUsb, u64)>,
    files: BTreeMap<String, Vec<u8>>,
    current: Option<String>,
    reading: VecDeque<u8>,
    manifest: Option<String>,
    notes: Vec<String>,
    /// flips a byte of this disk's copy when it is read back
    damage_copy: Option<&'static str>,
}

impl Edge {
    fn new(booted: Rc<Cell<bool>>, ram: Device) -> Edge {
        Edge {
            booted,
            ram: Shared(Rc::new(RefCell::new(ram))),
            opened: false,
            ram_ids: UsbIds {
                vendor_id: 0x1d6b,
                product_id: 0x0104,
                interfaces: vec![(0xff, 0x47, 0x52)],
            },
            asked: None,
            files: BTreeMap::new(),
            current: None,
            reading: VecDeque::new(),
            manifest: None,
            notes: vec![],
            damage_copy: None,
        }
    }
}

impl BackupEdge for Edge {
    type Ram = Shared;
    async fn wait_ram(&mut self, usb: &RamUsb, seconds: u64) -> Result<(Shared, UsbIds), String> {
        self.asked = Some((usb.clone(), seconds));
        if !self.booted.get() {
            return Err(format!("nothing with {usb} came up in {seconds} s"));
        }
        assert!(!self.opened, "opened twice");
        self.opened = true;
        Ok((self.ram.clone(), self.ram_ids.clone()))
    }
    fn begin(&mut self, disk: &Disk) -> Result<(), String> {
        let name = format!("{}.img.gz", disk.name);
        if self.files.contains_key(&name) {
            return Err(format!("{name} exists"));
        }
        self.files.insert(name.clone(), vec![]);
        self.current = Some(name);
        Ok(())
    }
    fn write(&mut self, data: &[u8]) -> Result<(), String> {
        let c = self.current.clone().unwrap();
        self.files.get_mut(&c).unwrap().extend_from_slice(data);
        Ok(())
    }
    fn end(&mut self, _disk: &Disk) -> Result<String, String> {
        Ok(self.current.take().unwrap())
    }
    fn reopen(&mut self, disk: &Disk) -> Result<(), String> {
        let mut b = self.files[&format!("{}.img.gz", disk.name)].clone();
        if self.damage_copy == Some(disk.name.as_str()) {
            let k = b.len() / 3;
            b[k] ^= 1;
        }
        self.reading = b.into();
        Ok(())
    }
    fn fill(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let n = buf.len().min(self.reading.len());
        for (i, b) in self.reading.drain(..n).enumerate() {
            buf[i] = b;
        }
        Ok(n)
    }
    fn manifest(&mut self, text: &str) -> Result<(), String> {
        self.manifest = Some(text.to_string());
        Ok(())
    }
    fn progress(&mut self, _disk: &Disk, _done: u64) {}
    fn note(&mut self, line: &str) {
        self.notes.push(line.to_string());
    }
}

fn descriptions() -> Vec<Description> {
    vec![valid(&noteair()), valid(&pc())]
}

fn fastboot_usb() -> UsbIds {
    UsbIds {
        vendor_id: 0x18d1,
        product_id: 0xd00d,
        interfaces: vec![(0xff, 0x42, 0x03)],
    }
}

fn host() -> Host {
    Host {
        platform: "linux".into(),
        connections: vec!["usb-native".into()],
    }
}

/// A boot image of an odd length, so the last transfer needs padding.
fn ram_image() -> Vec<u8> {
    let mut b = b"ANDROID!".to_vec();
    b.extend(ramsim::image(3 * LEN + 1234, 9));
    b
}

fn run_backup(boot: &mut Boot, edge: &mut Edge, image: &[u8], accept: bool) -> Result<BackupDone, BackupError> {
    let ds = descriptions();
    let refs: Vec<&Description> = ds.iter().collect();
    let opts = BackupOptions {
        image,
        accept_slot_flags: accept,
    };
    pollster::block_on(back_up_device(&refs, boot, &fastboot_usb(), &host(), opts, edge))
}

/// Before the boot, every command is a read.
fn only_reads(cmds: &[String]) {
    for c in cmds {
        let v = c.strip_prefix("getvar:").unwrap_or_else(|| panic!("not a read: {c:?}"));
        assert!(readable_variable(v).is_ok(), "{v}");
    }
}

fn setup(fixture: &str) -> (Boot, Edge) {
    let booted = Rc::new(Cell::new(false));
    let boot = Boot::from_fixture(fixture, booted.clone());
    let edge = Edge::new(booted, Device::new(ramsim::disks(), Faults::default()));
    (boot, edge)
}

#[test]
fn the_glassine_boot_loader_on_two_slots_is_refused_without_acceptance() {
    let (mut boot, mut edge) = setup("noteair-recorded-glassine-fastboot");
    let e = run_backup(&mut boot, &mut edge, &ram_image(), false).unwrap_err();
    let BackupError::SlotFlags(why) = &e else { panic!("{e}") };
    assert!(why.contains("reports 2 slots"), "{why}");
    assert!(why.contains("onyx-noteair mode fastboot says not to boot an image here"), "{why}");
    assert!(
        why.contains("clears the active slot's unbootable flag") && why.contains("primary and the backup GPT"),
        "{why}"
    );
    only_reads(&boot.commands());
    assert!(!boot.booted.get());
    assert!(edge.asked.is_none() && edge.files.is_empty() && edge.manifest.is_none());
}

#[test]
fn with_acceptance_every_disk_is_read_checked_and_stored() {
    let (mut boot, mut edge) = setup("noteair-recorded-glassine-fastboot");
    let image = ram_image();
    let done = run_backup(&mut boot, &mut edge, &image, true).unwrap();

    // fastboot: reads, then one download of the padded image and one boot
    let cmds = boot.commands();
    let k = cmds.iter().position(|c| c.starts_with("download:")).unwrap();
    only_reads(&cmds[..k]);
    let padded = image.len().div_ceil(LEN) * LEN;
    assert_eq!(cmds[k..], [format!("download:{padded:08x}"), "boot".to_string()]);
    assert!(boot.log[1..].iter().all(|t| t.len() == LEN), "every transfer after the opening is {LEN} bytes");
    assert_eq!(&boot.got[..image.len()], &image[..]);
    assert!(boot.got[image.len()..].iter().all(|&b| b == 0), "NUL padding");
    assert_eq!(done.prepared.download_size, padded as u64);
    assert!(
        edge.notes
            .iter()
            .any(|n| n.starts_with("accepted: booting the RAM environment changes the device"))
    );

    // the RAM environment: the description's ids, waited for 180 s
    let (usb, secs) = edge.asked.clone().unwrap();
    assert_eq!((usb.vendor_id, usb.product_id, secs), (0x1d6b, 0x0104, 180));

    // the copies are the disks, byte for byte, and the manifest says so
    let want = ramsim::disks();
    assert_eq!(edge.files.len(), want.len());
    let m: serde_json::Value = serde_json::from_str(edge.manifest.as_ref().unwrap()).unwrap();
    assert_eq!(m["kind"], "glassine-ram-backup");
    assert_eq!(m["description"], "onyx-noteair");
    assert_eq!(m["ram_image_sha256"], format!("{:x}", Sha256::digest(&image)));
    for (i, (name, data)) in want.iter().enumerate() {
        let file = format!("{name}.img.gz");
        assert_eq!(&edge.files[&file], data, "{name}");
        let d = &m["disks"][i];
        assert_eq!(d["name"], name.as_str());
        assert_eq!(d["size"], data.len() as u64);
        assert_eq!(d["file"], file);
        assert_eq!(d["sha256"], format!("{:x}", Sha256::digest(data)));
    }
    assert!(!edge.manifest.as_ref().unwrap().contains("serial"));
    assert!(done.rebooted);
    assert_eq!(done.disks.len(), want.len());
}

#[test]
fn the_ram_environment_is_only_read_and_then_rebooted_normally() {
    let (mut boot, mut edge) = setup("noteair-recorded-glassine-fastboot");
    run_backup(&mut boot, &mut edge, &ram_image(), true).unwrap();
    let d = edge.ram.0.borrow();
    // list, then per disk reads and hashes, then one reboot to the previous system
    assert_eq!(d.ops.first(), Some(&glassine_installer::glassine_read::OP_LIST));
    assert_eq!(d.ops.last(), Some(&glassine_installer::glassine_read::OP_REBOOT));
    assert_eq!(d.ops.iter().filter(|&&o| o == glassine_installer::glassine_read::OP_REBOOT).count(), 1);
    assert_eq!(d.rebooted, Some(0));
}

#[test]
fn the_stock_boot_loader_on_one_slot_needs_no_acceptance() {
    let (mut boot, mut edge) = setup("noteair-stock-fastboot");
    let done = run_backup(&mut boot, &mut edge, &ram_image(), false).unwrap();
    assert_eq!(done.prepared.mode, "fastboot-stock");
    assert!(done.prepared.slot_flags.is_none());
    assert!(!edge.notes.iter().any(|n| n.starts_with("accepted:")));
    assert_eq!(edge.files.len(), 3);
}

#[test]
fn not_a_boot_image_is_refused_before_download() {
    let (mut boot, mut edge) = setup("noteair-stock-fastboot");
    let e = run_backup(&mut boot, &mut edge, b"not an image at all", true).unwrap_err();
    assert!(matches!(e, BackupError::Refused(_)), "{e}");
    only_reads(&boot.commands());
}

#[test]
fn an_image_above_the_download_limit_is_refused_before_download() {
    let (mut boot, mut edge) = setup("noteair-stock-fastboot");
    boot.vars.insert("max-download-size".into(), "131070".into());
    let e = run_backup(&mut boot, &mut edge, &ram_image(), true).unwrap_err();
    assert!(e.to_string().contains("more than the boot loader takes (131070)"), "{e}");
    only_reads(&boot.commands());
}

#[test]
fn a_refused_boot_stops_before_waiting() {
    let (mut boot, mut edge) = setup("noteair-stock-fastboot");
    boot.refuse_boot = true;
    let e = run_backup(&mut boot, &mut edge, &ram_image(), true).unwrap_err();
    assert!(matches!(e, BackupError::Boot(_)), "{e}");
    assert!(edge.asked.is_none());
}

#[test]
fn a_ram_environment_that_never_comes_up_is_reported() {
    let (mut boot, mut edge) = setup("noteair-stock-fastboot");
    edge.booted = Rc::new(Cell::new(false)); // the edge never sees the boot
    let e = run_backup(&mut boot, &mut edge, &ram_image(), true).unwrap_err();
    assert_eq!(e.kind(), "no-ram-environment", "{e}");
    assert!(e.to_string().contains("1d6b:0104 (ff-47-52)"), "{e}");
}

#[test]
fn a_device_with_other_ids_is_not_read() {
    let (mut boot, mut edge) = setup("noteair-stock-fastboot");
    edge.ram_ids.product_id = 0x0105;
    let e = run_backup(&mut boot, &mut edge, &ram_image(), true).unwrap_err();
    assert_eq!(e.kind(), "no-ram-environment", "{e}");
    assert!(edge.files.is_empty());
}

#[test]
fn a_damaged_copy_fails_the_backup_and_writes_no_manifest() {
    let (mut boot, mut edge) = setup("noteair-stock-fastboot");
    edge.damage_copy = Some("mmcblk0boot0");
    let e = run_backup(&mut boot, &mut edge, &ram_image(), true).unwrap_err();
    assert!(e.to_string().contains("mmcblk0boot0: the stored copy differs"), "{e}");
    assert!(edge.manifest.is_none());
}

#[test]
fn a_corrupt_transfer_fails_the_backup() {
    let booted = Rc::new(Cell::new(false));
    let mut boot = Boot::from_fixture("noteair-stock-fastboot", booted.clone());
    let faults = Faults {
        corrupt_read: Some(2),
        ..Faults::default()
    };
    let mut edge = Edge::new(booted, Device::new(ramsim::disks(), faults));
    let e = run_backup(&mut boot, &mut edge, &ram_image(), true).unwrap_err();
    assert!(e.to_string().contains("doesn't match its checksum"), "{e}");
    assert!(edge.manifest.is_none());
}

#[test]
fn a_device_without_a_ram_mode_is_refused() {
    let ds = [valid(&mutate(
        &noteair(),
        "usb = { vendor_id = \"1d6b\", product_id = \"0104\", interface = \"ff-47-52\" }\n",
        "",
    ))];
    let refs: Vec<&Description> = ds.iter().collect();
    let booted = Rc::new(Cell::new(false));
    let mut boot = Boot::from_fixture("noteair-stock-fastboot", booted.clone());
    let mut edge = Edge::new(booted, Device::new(ramsim::disks(), Faults::default()));
    let opts = BackupOptions {
        image: &ram_image(),
        accept_slot_flags: true,
    };
    let e = pollster::block_on(back_up_device(&refs, &mut boot, &fastboot_usb(), &host(), opts, &mut edge)).unwrap_err();
    assert!(e.to_string().contains("has no USB ids with the interface ff-47-52"), "{e}");
    only_reads(&boot.commands());
}
