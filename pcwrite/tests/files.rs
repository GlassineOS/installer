// SPDX-License-Identifier: GPL-2.0-or-later
//! The PC writer on a tree of plain files standing for a machine: the
//! reader inspects it, the planner plans, the core's PC executor runs with
//! this crate's machine and edge, and the files are checked afterwards. The
//! PC description with its sizes scaled down (2 GiB → 40 MiB, 16 GiB → 64
//! MiB, 8 GiB → 16 MiB) keeps the disks small.

#[path = "../../core/tests/common/img.rs"]
mod img;

use glassine_installer::artifacts::{ArtifactSource, Artifacts};
use glassine_installer::efivar;
use glassine_installer::execute::Outcome;
use glassine_installer::fat;
use glassine_installer::format1::Description;
use glassine_installer::lock::sha256_hex;
use glassine_installer::pc_execute::{Machine, PcJob, run};
use glassine_installer::plan::{Input, plan};
use glassine_installer::recognize::recognize;
use glassine_installer::record::{Confirmation, Record};
use glassine_installer::table;
use glassine_installer::validate::{Loaded, load};
use glassine_installer_pcwrite::{DirEdge, LinuxMachine};
use img::MIB;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

const DISK: u64 = 256 * MIB;

fn tree(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn put(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

/// A machine: a blank NVMe disk, the installer's USB stick, a firmware with
/// two entries.
fn machine(root: &Path) {
    for (name, link, size, major) in [
        ("nvme0n1", "devices/pci0000:00/nvme/nvme0/nvme0n1", DISK, 259),
        ("sda", "devices/pci0000:00/usb1/1-1/host0/block/sda", 64 * MIB, 8),
    ] {
        let dev = format!("sys/{link}");
        put(root, &format!("{dev}/size"), format!("{}\n", size / 512).as_bytes());
        put(root, &format!("{dev}/removable"), b"0\n");
        put(root, &format!("{dev}/dev"), format!("{major}:0\n").as_bytes());
        put(root, &format!("{dev}/queue/logical_block_size"), b"512\n");
        fs::create_dir_all(root.join("sys/block")).unwrap();
        std::os::unix::fs::symlink(format!("../{link}"), root.join("sys/block").join(name)).unwrap();
        fs::create_dir_all(root.join("dev")).unwrap();
        File::create(root.join("dev").join(name)).unwrap().set_len(size).unwrap();
    }
    File::options()
        .write(true)
        .open(root.join("dev/sda"))
        .unwrap()
        .write_all_at(b"the stick", 0)
        .unwrap();
    let g = efivar::GLOBAL;
    let vars = root.join("sys/firmware/efi/efivars");
    fs::create_dir_all(&vars).unwrap();
    fs::write(vars.join(format!("Boot0000-{g}")), img::load_option("UEFI Shell", None, "\\shell.efi")).unwrap();
    fs::write(vars.join(format!("BootOrder-{g}")), efivar::numbers_file(&[0])).unwrap();
    fs::write(vars.join(format!("SecureBoot-{g}")), [6, 0, 0, 0, 0]).unwrap();
    put(root, "proc/self/mountinfo", b"");
    put(root, "proc/sys/kernel/arch", b"x86_64\n");
}

fn small_pc() -> String {
    let t = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../devices/uefi-pc.toml")).unwrap();
    t.replace("size = \"2GiB\"", "size = \"40MiB\"")
        .replace("size = \"16GiB\"", "size = \"64MiB\"")
        .replace("at_least = \"8GiB\"", "at_least = \"16MiB\"")
}

fn image(len: usize, seed: u32) -> Vec<u8> {
    (0..len).map(|i| ((i as u32).wrapping_mul(2654435761) >> seed) as u8).collect()
}

struct Dir(PathBuf);
impl ArtifactSource for Dir {
    fn origin(&self) -> String {
        self.0.display().to_string()
    }
    async fn size(&mut self, f: &str) -> Result<u64, String> {
        fs::metadata(self.0.join(f)).map(|m| m.len()).map_err(|e| e.to_string())
    }
    async fn read_at(&mut self, f: &str, o: u64, b: &mut [u8]) -> Result<(), String> {
        File::open(self.0.join(f)).and_then(|x| x.read_exact_at(b, o)).map_err(|e| e.to_string())
    }
}

fn release(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    let mut m = serde_json::Map::new();
    for (role, len, kind, seed) in [
        ("base", 3 * MIB as usize + 512, "btrfs-pool-image", 3),
        ("kernel", 2 * MIB as usize + 77, "efi-kernel", 5),
        ("initramfs", MIB as usize + 3, "initramfs", 7),
        ("boot-loader", 70_000, "efi-application", 9),
    ] {
        let b = image(len, seed);
        let file = format!("{role}.img");
        fs::write(dir.join(&file), &b).unwrap();
        let mut e = serde_json::json!({"file": file, "sha256": sha256_hex(&b), "size": len, "kind": kind});
        if role == "kernel" {
            e["cmdline"] = "console=ttyS0".into();
        }
        m.insert(role.into(), e);
    }
    fs::write(
        dir.join("release.json"),
        serde_json::to_vec(&serde_json::json!({"release_format": 1, "artifacts": m})).unwrap(),
    )
    .unwrap();
}

fn block<F: Future>(f: F) -> F::Output {
    pollster::block_on(f)
}

#[test]
fn a_tree_of_files_is_installed_with_its_backup_and_other_disks_untouched() {
    let root = tree("install");
    machine(&root);
    let rel = root.join("release");
    release(&rel);
    let text = small_pc();
    let d: Description = match load(&text) {
        Loaded::Valid(d) => *d,
        _ => panic!(),
    };
    let facts = glassine_installer_pc::inspect(&root).unwrap().snapshot;
    let ds = [&d];
    let rec = recognize(&ds, &facts);
    let mut a = block(Artifacts::open(Dir(rel.clone()))).unwrap();
    let p = plan(
        &ds,
        &facts,
        &rec,
        &Input {
            choices: BTreeMap::from([("target-disk".into(), "disk0".into())]),
            developer: true,
            release: Some(a.manifest.clone()),
            ..Default::default()
        },
    );
    let plan_json = serde_json::to_string_pretty(&p).unwrap();
    let arts = a.roles().iter().map(|r| a.artifact(r).unwrap()).collect();
    let mut record = Record::new(
        "test",
        &d.id,
        text.as_bytes(),
        &plan_json,
        Confirmation {
            text: "install".into(),
            at: None,
        },
        arts,
        None,
    )
    .unwrap();
    let stick_before = fs::read(root.join("dev/sda")).unwrap();
    let mut m = LinuxMachine::new(&root, 0).unwrap();
    // the stick (not the plan's target) is never written
    assert!(m.write(1, 0, b"x").unwrap_err().contains("refused"));
    let mut e = DirEdge::new(&root.join("record.json"), Some(root.join("backup")), None);
    let job = PcJob {
        description: &d,
        description_sha256: sha256_hex(text.as_bytes()),
        developer: true,
    };
    let r = block(run(&job, &plan_json, &mut record, &mut a, &mut m, &mut e));
    assert!(matches!(r, Ok(Outcome::WaitingForAcceptance { .. })), "{r:?}\n{plan_json}");
    drop(m);
    // the backup: the disk compressed, its manifest last, as the record holds
    let manifest = fs::read(root.join("backup/manifest.json")).unwrap();
    assert_eq!(record.backup.as_ref().unwrap().manifest_sha256.as_deref(), Some(sha256_hex(&manifest).as_str()));
    let gz = fs::metadata(root.join("backup/disk0.img.gz")).unwrap().len();
    assert!(gz < 2 * MIB, "a blank 256 MiB disk compresses ({gz} bytes)");
    // the saved record is the one in memory
    assert_eq!(fs::read_to_string(root.join("record.json")).unwrap(), record.to_json());
    assert!(root.join("record.json.kept/pc-before.json").is_file());
    // the disk: three partitions, the base, a boot volume a firmware reads
    let disk = fs::read(root.join("dev/nvme0n1")).unwrap();
    let t = table::read(&mut disk.as_slice(), DISK, 512).unwrap();
    assert!(t.primary_ok);
    let names: Vec<_> = t.entries.iter().map(|e| e.name.clone().unwrap_or_default()).collect();
    assert_eq!(names, ["glassine-boot", "glassine-system", "glassine-userdata"]);
    let base = fs::read(rel.join("base.img")).unwrap();
    let sys = &t.entries[1];
    assert_eq!(&disk[sys.start as usize..sys.start as usize + base.len()], base.as_slice());
    let boot = &t.entries[0];
    let vol = &disk[boot.start as usize..(boot.start + boot.size) as usize];
    assert_eq!(
        fat::read::file(vol, "EFI/BOOT/BOOTX64.EFI").unwrap(),
        fs::read(rel.join("boot-loader.img")).unwrap()
    );
    let lr = String::from_utf8(fat::read::file(vol, "glassine/loader.rec").unwrap()).unwrap();
    assert!(lr.contains(&format!("glassine.system=PARTUUID={}", sys.guid.as_deref().unwrap())), "{lr}");
    // the firmware: ours first, the shell after it, unchanged
    let vars = root.join("sys/firmware/efi/efivars");
    let order = efivar::number_list(&fs::read(vars.join(efivar::global_name("BootOrder"))).unwrap());
    assert_eq!(order, [1, 0]);
    let ours = efivar::load_option(&fs::read(vars.join(efivar::boot_name(1))).unwrap()).unwrap();
    assert_eq!(ours.label, "GlassineOS");
    assert_eq!(fs::read(root.join("dev/sda")).unwrap(), stick_before);
}

#[test]
fn a_disk_in_use_is_refused() {
    let root = tree("in-use");
    machine(&root);
    // a partition of the target mounted (its dev number 259:1)
    fs::create_dir_all(root.join("sys/devices/pci0000:00/nvme/nvme0/nvme0n1/nvme0n1p1")).unwrap();
    put(&root, "sys/devices/pci0000:00/nvme/nvme0/nvme0n1/nvme0n1p1/partition", b"1\n");
    put(&root, "sys/devices/pci0000:00/nvme/nvme0/nvme0n1/nvme0n1p1/dev", b"259:1\n");
    put(&root, "proc/self/mountinfo", b"30 1 259:1 / /mnt rw - ext4 /dev/nvme0n1p1 rw\n");
    let e = LinuxMachine::new(&root, 0).err().unwrap();
    assert!(e.contains("in use"), "{e}");
    // the stick, mounted: still a machine whose target is disk 0 only
    put(&root, "proc/self/mountinfo", b"");
    assert!(LinuxMachine::new(&root, 0).is_ok());
}

#[test]
fn a_backup_store_reads_back_what_it_stored() {
    use glassine_installer::pc_execute::PcEdge;
    let root = tree("store");
    let mut e = DirEdge::new(&root.join("r.json"), Some(root.join("b")), Some(root.join("cancel")));
    e.backup_begin("disk0.img").unwrap();
    let data = image(3 * MIB as usize, 4);
    for c in data.chunks(100_000) {
        e.backup_append(c).unwrap();
    }
    e.backup_end().unwrap();
    let mut back = vec![];
    e.backup_read("disk0.img", &mut |b| {
        back.extend_from_slice(b);
        Ok(())
    })
    .unwrap();
    assert_eq!(back, data);
    assert!(root.join("b/disk0.img.gz").is_file());
    // an image larger than a part goes on in more files, read back as one
    e.segment = 100_000;
    let noise: Vec<u8> = (0..2 * MIB as u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    e.backup_begin("disk1.img").unwrap();
    e.backup_append(&noise).unwrap();
    e.backup_end().unwrap();
    assert!(root.join("b/disk1.img.gz.1").is_file() && fs::metadata(root.join("b/disk1.img.gz")).unwrap().len() == 100_000);
    let mut back = vec![];
    e.backup_read("disk1.img", &mut |b| {
        back.extend_from_slice(b);
        Ok(())
    })
    .unwrap();
    assert_eq!(back, noise);
    // a shorter image written again leaves no stale parts behind
    e.backup_begin("disk1.img").unwrap();
    e.backup_append(b"short").unwrap();
    e.backup_end().unwrap();
    assert!(!root.join("b/disk1.img.gz.1").exists());
    e.segment = glassine_installer_pcwrite::SEGMENT;
    e.backup_put("manifest.json", b"{}").unwrap();
    assert_eq!(e.backup_get("manifest.json").unwrap().unwrap(), b"{}");
    assert!(e.backup_put("../x", b"").is_err());
    e.keep("pc-before.json", b"k").unwrap();
    assert_eq!(e.kept("pc-before.json").unwrap().unwrap(), b"k");
    assert!(!e.cancel_requested());
    fs::write(root.join("cancel"), b"").unwrap();
    assert!(e.cancel_requested());
}

#[test]
fn a_disk_of_an_odd_size_is_read_to_its_last_sector() {
    let root = tree("odd");
    machine(&root);
    // 256 MiB and one sector: the last 512 bytes aren't a whole 4 KiB
    let size = DISK + 512;
    put(&root, "sys/devices/pci0000:00/nvme/nvme0/nvme0n1/size", format!("{}\n", size / 512).as_bytes());
    File::options().write(true).open(root.join("dev/nvme0n1")).unwrap().set_len(size).unwrap();
    let mut m = LinuxMachine::new(&root, 0).unwrap();
    assert_eq!(m.disks().unwrap()[0].size, size);
    m.write(0, size - 512, &[7u8; 512]).unwrap();
    m.write(0, size - 4096 - 512, &[9u8; 4096]).unwrap();
    m.flush(0).unwrap();
    let mut last = vec![0u8; 4608];
    m.read(0, size - 4608, &mut last).unwrap();
    assert!(last[..4096].iter().all(|b| *b == 9) && last[4096..].iter().all(|b| *b == 7));
    let mut one = [0u8; 100];
    m.read(0, size - 100, &mut one).unwrap();
    assert_eq!(one, [7u8; 100]);
    assert!(m.read(0, size - 50, &mut one).is_err(), "past the end");
}
