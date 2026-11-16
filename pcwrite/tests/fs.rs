// SPDX-License-Identifier: GPL-2.0-or-later
//! The backup image on real file systems: a disk with FAT32, FAT16, FAT12,
//! ext4, btrfs and NTFS partitions and one the installer doesn't read, each
//! made by its own tools (mkfs.fat, mkfs.ext4, mkfs.btrfs, mkntfs) and filled
//! with files, is backed up through the PC writer, restored onto a blank
//! disk, and each file system is checked with its fsck and read back
//! without mounting (mtools, debugfs, btrfs restore, ntfscat), file by file.
//!
//! The tools are needed: without them the tests say so and pass, unless
//! `GLASSINE_FS_FIXTURES=require` (the CI image has them).

#[path = "../../core/tests/common/img.rs"]
mod img;

use glassine_installer::pc_execute::{DiskId, Machine};
use glassine_installer::pc_image::{self, ImageSummary};
use glassine_installer::used;
use glassine_installer_pcwrite::{DirEdge, LinuxMachine};
use img::{MIB, Part};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const DISK: u64 = 512 * MIB;
const TOOLS: &[&str] = &[
    "mkfs.fat",
    "fsck.fat",
    "mcopy",
    "mkfs.ext4",
    "e2fsck",
    "debugfs",
    "mkfs.btrfs",
    "btrfs",
    "mkntfs",
    "ntfscp",
    "ntfscat",
    "ntfsfix",
];

fn have_tools() -> bool {
    let missing: Vec<_> = TOOLS
        .iter()
        .filter(|t| !std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).any(|d| d.join(t).is_file()))
        .collect();
    if missing.is_empty() {
        return true;
    }
    if std::env::var("GLASSINE_FS_FIXTURES").as_deref() == Ok("require") {
        panic!("the file-system tools are missing: {missing:?}");
    }
    eprintln!("skipped: the file-system tools are missing: {missing:?}");
    false
}

/// A folder of the test's own, removed when the test ends.
struct Work(PathBuf);
impl Work {
    fn new(name: &str) -> Work {
        let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("fs-{name}"));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        Work(d)
    }
}
impl Drop for Work {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(cmd: &str, args: &[&str]) -> String {
    let o = Command::new(cmd).args(args).output().unwrap_or_else(|e| panic!("{cmd}: {e}"));
    assert!(
        o.status.success(),
        "{cmd} {args:?}: {}\n{}\n{}",
        o.status,
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn put(root: &Path, rel: &str, bytes: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

/// A machine below `root` with one NVMe disk, `dev/nvme0n1`.
fn machine(root: &Path) -> PathBuf {
    let link = "devices/pci0000:00/nvme/nvme0/nvme0n1";
    put(root, &format!("sys/{link}/size"), format!("{}\n", DISK / 512).as_bytes());
    put(root, &format!("sys/{link}/removable"), b"0\n");
    put(root, &format!("sys/{link}/dev"), b"259:0\n");
    put(root, &format!("sys/{link}/queue/logical_block_size"), b"512\n");
    fs::create_dir_all(root.join("sys/block")).unwrap();
    std::os::unix::fs::symlink(format!("../{link}"), root.join("sys/block/nvme0n1")).unwrap();
    fs::create_dir_all(root.join("sys/firmware/efi/efivars")).unwrap();
    put(root, "proc/self/mountinfo", b"");
    put(root, "proc/sys/kernel/arch", b"x86_64\n");
    let dev = root.join("dev/nvme0n1");
    fs::create_dir_all(dev.parent().unwrap()).unwrap();
    File::create(&dev).unwrap().set_len(DISK).unwrap();
    dev
}

fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

/// The files every file system gets: name → bytes (NTFS gets only the top level).
fn files() -> BTreeMap<String, Vec<u8>> {
    let mut f = BTreeMap::new();
    f.insert("hello.txt".to_string(), b"hello from the backup test\n".to_vec());
    f.insert("big.bin".to_string(), noise(3 * MIB as usize + 777, 1));
    f.insert("empty".to_string(), vec![]);
    f.insert("dir/inner.bin".to_string(), noise(200_000, 2));
    f.insert("dir/sub/deep.txt".to_string(), b"deep\n".to_vec());
    f
}

fn write_tree(dir: &Path, files: &BTreeMap<String, Vec<u8>>) {
    for (n, b) in files {
        put(dir, n, b);
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Fs {
    Fat32,
    Fat16,
    Fat12,
    Ext4,
    Btrfs,
    Ntfs,
    /// bytes the installer doesn't read: kept whole
    Other,
}

/// The disk's partitions: (file system, first LBA, size).
const LAYOUT: &[(Fs, u64, u64)] = &[
    (Fs::Fat32, 2048, 64 * MIB),
    (Fs::Fat16, 2048 + 64 * 2048, 32 * MIB),
    (Fs::Fat12, 2048 + 96 * 2048, 4 * MIB),
    (Fs::Ext4, 2048 + 100 * 2048, 96 * MIB),
    (Fs::Btrfs, 2048 + 196 * 2048, 160 * MIB),
    (Fs::Ntfs, 2048 + 356 * 2048, 64 * MIB),
    (Fs::Other, 2048 + 420 * 2048, 8 * MIB),
];

/// A file system of `size` bytes in the file `img`, holding `files`. With
/// `dirty`, the space under it is noise first, as a disk in use holds.
fn make_fs(fs_: Fs, img: &Path, size: u64, files: &BTreeMap<String, Vec<u8>>, src: &Path, dirty: bool) {
    let f = File::create(img).unwrap();
    f.set_len(size).unwrap();
    if dirty {
        for (i, at) in (0..size).step_by(MIB as usize).enumerate() {
            f.write_all_at(&noise(MIB as usize, 100 + i as u64), at).unwrap();
        }
    }
    drop(f);
    let p = img.to_str().unwrap();
    match fs_ {
        Fs::Fat32 | Fs::Fat16 | Fs::Fat12 => {
            let (bits, spc) = match fs_ {
                Fs::Fat32 => ("32", "1"),
                Fs::Fat16 => ("16", "1"),
                _ => ("12", "4"),
            };
            run("mkfs.fat", &["-F", bits, "-s", spc, "-n", "TEST", p]);
            let mut args = vec!["-s", "-i", p];
            let tops: Vec<String> = fs::read_dir(src).unwrap().map(|e| e.unwrap().path().display().to_string()).collect();
            args.extend(tops.iter().map(String::as_str));
            args.push("::/");
            run("mcopy", &args);
        }
        Fs::Ext4 => {
            run("mkfs.ext4", &["-q", "-F", "-E", "nodiscard", "-d", src.to_str().unwrap(), p]);
        }
        Fs::Btrfs => {
            run("mkfs.btrfs", &["-q", "-K", "-f", "-r", src.to_str().unwrap(), p]);
        }
        Fs::Ntfs => {
            run("mkntfs", &["-q", "-F", "-Q", p]);
            for (n, _) in files.iter().filter(|(n, _)| !n.contains('/')) {
                run("ntfscp", &["-f", p, src.join(n).to_str().unwrap(), n]);
            }
        }
        Fs::Other => {
            let f = File::options().write(true).open(img).unwrap();
            f.write_all_at(b"a partition this installer doesn't read", 4096).unwrap();
            f.write_all_at(&noise(100_000, 7), 3 * MIB).unwrap();
        }
    }
}

/// Copies `src` into `dst` at `at`, skipping blocks of zeros (`dst` stays sparse).
fn copy_in(src: &Path, dst: &Path, at: u64) {
    let s = File::open(src).unwrap();
    let d = File::options().write(true).open(dst).unwrap();
    let len = s.metadata().unwrap().len();
    let mut buf = vec![0u8; 64 * 1024];
    let mut o = 0;
    while o < len {
        let n = (len - o).min(buf.len() as u64) as usize;
        s.read_exact_at(&mut buf[..n], o).unwrap();
        if buf[..n].iter().any(|b| *b != 0) {
            d.write_all_at(&buf[..n], at + o).unwrap();
        }
        o += n as u64;
    }
}

/// `len` bytes of `dev` at `at` into a file of their own.
fn copy_out(dev: &Path, at: u64, len: u64, out: &Path) {
    let s = File::open(dev).unwrap();
    let d = File::create(out).unwrap();
    d.set_len(len).unwrap();
    let mut buf = vec![0u8; 64 * 1024];
    let mut o = 0;
    while o < len {
        let n = (len - o).min(buf.len() as u64) as usize;
        s.read_exact_at(&mut buf[..n], at + o).unwrap();
        if buf[..n].iter().any(|b| *b != 0) {
            d.write_all_at(&buf[..n], o).unwrap();
        }
        o += n as u64;
    }
}

/// Makes the disk: the GPT, then each partition's file system.
fn make_disk(w: &Path, dev: &Path, dirty: bool) -> BTreeMap<String, Vec<u8>> {
    let files = files();
    let src = w.join("src");
    write_tree(&src, &files);
    let parts: Vec<Part> = LAYOUT
        .iter()
        .map(|(f, lba, size)| Part {
            type_guid: if matches!(f, Fs::Ext4 | Fs::Btrfs) { img::LINUX } else { img::BASIC },
            first_lba: *lba,
            last_lba: lba + size / 512 - 1,
            name: "",
        })
        .collect();
    let d = File::options().write(true).open(dev).unwrap();
    for (off, b) in img::gpt(DISK, 512, &parts) {
        d.write_all_at(&b, off).unwrap();
    }
    drop(d);
    for (i, (f, lba, size)) in LAYOUT.iter().enumerate() {
        let one = w.join(format!("part{i}.img"));
        make_fs(*f, &one, *size, &files, &src, dirty);
        copy_in(&one, dev, lba * 512);
        fs::remove_file(&one).unwrap();
    }
    files
}

fn sha_file(p: &Path) -> String {
    let mut h = Sha256::new();
    h.update(fs::read(p).unwrap());
    hex(&h.finalize())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Every file system on the restored disk: its fsck passes and every file
/// reads back as it was made.
fn check_restored(w: &Path, dev: &Path, files: &BTreeMap<String, Vec<u8>>) {
    for (i, (f, lba, size)) in LAYOUT.iter().enumerate() {
        let one = w.join(format!("restored{i}.img"));
        copy_out(dev, lba * 512, *size, &one);
        let p = one.to_str().unwrap();
        let out = w.join(format!("tree{i}"));
        fs::create_dir_all(&out).unwrap();
        let o = out.to_str().unwrap();
        let mut want: Vec<&String> = files.keys().collect();
        match f {
            Fs::Fat32 | Fs::Fat16 | Fs::Fat12 => {
                run("fsck.fat", &["-n", p]);
                run("mcopy", &["-s", "-n", "-i", p, "::/", o]);
            }
            Fs::Ext4 => {
                run("e2fsck", &["-fn", p]);
                run("debugfs", &["-R", &format!("rdump / {o}"), p]);
            }
            Fs::Btrfs => {
                run("btrfs", &["check", "--readonly", p]);
                run("btrfs", &["restore", p, o]);
            }
            Fs::Ntfs => {
                run("ntfsfix", &["-n", p]);
                want.retain(|n| !n.contains('/'));
                for n in &want {
                    let b = Command::new("ntfscat").args([p, n.as_str()]).output().unwrap();
                    assert!(b.status.success(), "ntfscat {n}");
                    put(&out, n, &b.stdout);
                }
            }
            Fs::Other => {
                fs::remove_file(&one).unwrap();
                continue;
            }
        }
        for n in want {
            let got = out.join(n);
            assert!(got.is_file(), "{f:?}: {n} is missing after the restore");
            assert_eq!(sha_file(&got), hex(&Sha256::digest(&files[n])), "{f:?}: {n} differs after the restore");
        }
        fs::remove_file(&one).unwrap();
        fs::remove_dir_all(&out).unwrap();
    }
}

fn disk_id(m: &mut LinuxMachine) -> DiskId {
    m.disks().unwrap().into_iter().find(|d| d.index == 0).unwrap()
}

/// Backs the disk up into `w/backup`; restores it onto the same file
/// emptied (a blank disk). Returns the image's summary.
fn back_up_and_restore(w: &Path, root: &Path, dev: &Path) -> ImageSummary {
    let mut m = LinuxMachine::new(root, 0).unwrap();
    let mut e = DirEdge::new(&w.join("record.json"), Some(w.join("backup")), None);
    let id = disk_id(&mut m);
    let bd = pc_image::back_up_disk(&mut m, &mut e, "back-up", &id, "disk0.img").unwrap();
    drop(m);
    // a blank disk of the same size
    fs::remove_file(dev).unwrap();
    File::create(dev).unwrap().set_len(DISK).unwrap();
    let mut m = LinuxMachine::new(root, 0).unwrap();
    let r = pc_image::restore_disk(&mut m, &mut e, &bd, 2, 0).unwrap();
    assert!(r.written_bytes > 0);
    // a second run finds it all in place
    let r = pc_image::restore_disk(&mut m, &mut e, &bd, 2, 0).unwrap();
    assert_eq!(r.written_bytes, 0);
    bd.image.unwrap()
}

#[test]
fn file_systems_on_clean_space_come_back_byte_for_byte() {
    if !have_tools() {
        return;
    }
    let w = Work::new("clean");
    let root = w.0.join("root");
    let dev = machine(&root);
    let files = make_disk(&w.0, &dev, false);
    let before = w.0.join("before.img");
    fs::copy(&dev, &before).unwrap();
    let im = back_up_and_restore(&w.0, &root, &dev);
    for p in &im.partitions {
        let other = p.fs.is_none();
        assert_eq!(p.kept.starts_with("whole"), other, "{p:?}");
    }
    // Built on zeros: every byte the file systems wrote is in the image, so
    // the blank disk restored is the disk as it was. Not btrfs's partition:
    // mkfs.btrfs builds the file system in temporary chunks first (at 1 and
    // 5 MiB) and leaves their blocks behind outside every chunk the file
    // system has (its device extents say so); btrfs check passes on the
    // restored copy without them (check_restored).
    let (_, blba, bsize) = LAYOUT[4];
    let btrfs = blba * 512..blba * 512 + bsize;
    let (a, b) = (File::open(&before).unwrap(), File::open(&dev).unwrap());
    let mut x = vec![0u8; MIB as usize];
    let mut y = vec![0u8; MIB as usize];
    for at in (0..DISK).step_by(MIB as usize) {
        if btrfs.contains(&at) {
            continue;
        }
        a.read_exact_at(&mut x, at).unwrap();
        b.read_exact_at(&mut y, at).unwrap();
        assert!(x == y, "the restored disk differs in the MiB at {at}: {:?}", im.partitions);
    }
    check_restored(&w.0, &dev, &files);
}

#[test]
fn free_space_is_left_out_and_the_file_systems_come_back_whole() {
    if !have_tools() {
        return;
    }
    let w = Work::new("dirty");
    let root = w.0.join("root");
    let dev = machine(&root);
    let files = make_disk(&w.0, &dev, true);
    let im = back_up_and_restore(&w.0, &root, &dev);
    eprintln!("{im:#?}");
    for p in &im.partitions {
        assert_eq!(p.kept.starts_with("whole"), p.fs.is_none(), "{p:?}");
        // The free space of these small file systems is most of them. btrfs
        // is kept by its chunks (allocated space, not the extents in it):
        // on 160 MiB, mkfs.btrfs allocates about 90 MiB of them.
        let bound = if p.fs.as_deref() == Some("btrfs") { p.size * 6 / 10 } else { p.size / 2 };
        if p.fs.is_some() && p.size >= 32 * MIB {
            assert!(p.bytes < bound, "{p:?}");
        }
    }
    assert!(im.data_bytes < DISK / 3, "{im:?}");
    check_restored(&w.0, &dev, &files);
}

/// Each file system's metadata made to fail a check: the partition is kept whole.
#[test]
fn damaged_metadata_keeps_the_partition_whole() {
    if !have_tools() {
        return;
    }
    let w = Work::new("damaged");
    let files = files();
    let src = w.0.join("src");
    write_tree(&src, &files);
    let flip = |p: &Path, at: u64| {
        let f = File::options().read(true).write(true).open(p).unwrap();
        let mut b = [0u8];
        f.read_exact_at(&mut b, at).unwrap();
        f.write_all_at(&[b[0] ^ 0x5a], at).unwrap();
    };
    let le = |b: &[u8], o: usize, n: usize| b[o..o + n].iter().rev().fold(0u64, |a, x| a << 8 | *x as u64);
    for (f, size) in [(Fs::Fat32, 64 * MIB), (Fs::Ext4, 96 * MIB), (Fs::Btrfs, 160 * MIB), (Fs::Ntfs, 64 * MIB)] {
        let one = w.0.join("one.img");
        make_fs(f, &one, size, &files, &src, false);
        let mut r = fs::read(&one).unwrap();
        let a = used::partition(&mut r.as_slice(), 0, size);
        assert!(a.used.is_ok(), "{f:?} reads when sound: {a:?}");
        let head = r[..4096].to_vec();
        match f {
            // a byte of the second FAT: the copies disagree
            Fs::Fat32 => {
                let (bps, reserved, fatsz) = (le(&head, 11, 2), le(&head, 14, 2), le(&head, 36, 4));
                flip(&one, (reserved + fatsz) * bps + 8 * 4 + 1);
            }
            // a byte of group 0's descriptor: its checksum fails
            Fs::Ext4 => {
                let bs = 1024u64 << le(&head, 1024 + 0x18, 4);
                let gdt = if bs == 1024 { 2048 } else { bs };
                flip(&one, gdt + 0x0c);
            }
            // a byte of the superblock's label: its checksum fails
            Fs::Btrfs => flip(&one, 0x10000 + 0x12b + 3),
            // a byte of $MFTMirr: it and $MFT disagree
            Fs::Ntfs => {
                let cluster = le(&head, 11, 2) * head[13] as u64;
                flip(&one, le(&head, 0x38, 8) * cluster + 0x30);
            }
            _ => unreachable!(),
        }
        r = fs::read(&one).unwrap();
        let a = used::partition(&mut r.as_slice(), 0, size);
        let why = a.used.expect_err(&format!("{f:?} damaged is kept whole"));
        assert!(why.contains("kept whole"), "{f:?}: {why}");
        eprintln!("{f:?}: {why}");
        fs::remove_file(&one).unwrap();
    }
    // a journal not replayed, a volume marked dirty
    let one = w.0.join("one.img");
    make_fs(Fs::Ext4, &one, 96 * MIB, &files, &src, false);
    run("debugfs", &["-w", "-R", "feature needs_recovery", one.to_str().unwrap()]);
    let r = fs::read(&one).unwrap();
    let why = used::partition(&mut r.as_slice(), 0, 96 * MIB).used.unwrap_err();
    assert!(why.contains("journal"), "{why}");
}

/// A damaged file system on the disk: the image holds the whole partition
/// and the restore brings it back as it was, damage included.
#[test]
fn a_partition_kept_whole_is_restored_whole() {
    if !have_tools() {
        return;
    }
    let w = Work::new("whole");
    let root = w.0.join("root");
    let dev = machine(&root);
    make_disk(&w.0, &dev, true);
    // ext4's superblock state: not cleanly unmounted
    let (_, lba, size) = LAYOUT[3];
    let f = File::options().read(true).write(true).open(&dev).unwrap();
    f.write_all_at(&[0, 0], lba * 512 + 1024 + 0x3a).unwrap();
    drop(f);
    let one = w.0.join("ext4-before.img");
    copy_out(&dev, lba * 512, size, &one);
    let before = sha_file(&one);
    let im = back_up_and_restore(&w.0, &root, &dev);
    let p = im.partitions.iter().find(|p| p.start == lba * 512).unwrap();
    assert!(p.kept.contains("not cleanly unmounted") && p.bytes == size, "{p:?}");
    copy_out(&dev, lba * 512, size, &one);
    assert_eq!(sha_file(&one), before, "the ext4 partition byte for byte");
}

struct FileAt(File);
impl glassine_installer::table::ReadAt for FileAt {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        self.0.read_exact_at(buf, offset).map_err(|e| e.to_string())
    }
}

/// ext4 as it is made for real disks (4 KiB blocks, several block groups,
/// flex_bg, uninitialized groups): only the ranges its bitmaps mark used,
/// copied onto zeros, are a file system that checks clean and holds the files.
#[test]
fn ext4_with_4k_blocks_comes_back_from_its_used_blocks_alone() {
    if !have_tools() {
        return;
    }
    let w = Work::new("ext4-4k");
    let size = 1024 * MIB;
    let src = w.0.join("src");
    let files = files();
    write_tree(&src, &files);
    let img = w.0.join("ext4.img");
    File::create(&img).unwrap().set_len(size).unwrap();
    run(
        "mkfs.ext4",
        &["-q", "-F", "-b", "4096", "-E", "nodiscard", "-d", src.to_str().unwrap(), img.to_str().unwrap()],
    );
    let a = used::partition(&mut FileAt(File::open(&img).unwrap()), 0, size);
    assert_eq!(a.kind.as_deref(), Some("ext4"));
    let ranges = a.used.unwrap_or_else(|e| panic!("kept whole: {e}"));
    let kept: u64 = ranges.iter().map(|(s, e)| e - s).sum();
    assert!(kept < size / 4, "{kept} of {size} bytes kept");
    // only those ranges onto a blank (zero) file
    let out = w.0.join("restored.img");
    let (from, to) = (File::open(&img).unwrap(), File::create(&out).unwrap());
    to.set_len(size).unwrap();
    for (s, e) in &ranges {
        let mut at = *s;
        while at < *e {
            let n = (*e - at).min(4 * MIB) as usize;
            let mut b = vec![0u8; n];
            from.read_exact_at(&mut b, at).unwrap();
            to.write_all_at(&b, at).unwrap();
            at += n as u64;
        }
    }
    drop((from, to));
    let p = out.to_str().unwrap();
    run("e2fsck", &["-fn", p]);
    let tree = w.0.join("tree");
    fs::create_dir_all(&tree).unwrap();
    run("debugfs", &["-R", &format!("rdump / {}", tree.display()), p]);
    for (n, b) in &files {
        assert_eq!(sha_file(&tree.join(n)), hex(&Sha256::digest(b)), "{n} differs");
    }
}
