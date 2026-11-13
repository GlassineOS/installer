// SPDX-License-Identifier: GPL-2.0-or-later
//! The PC reader on a fake tree: sysfs as text files, the disks as sparse
//! image files made here, efivarfs files, mountinfo and the program's link.
//! Then the plan on what it read: the installer's own disk refused, a disk
//! too small refused with what it lacks, the image bounds the file systems
//! tighten.
#![allow(clippy::disallowed_methods, clippy::disallowed_types)] // the tests build the trees

#[path = "../../core/tests/common/img.rs"]
mod img;

use glassine_installer::format1::Description;
use glassine_installer::plan::{self, Input, Status};
use glassine_installer::recognize::recognize;
use glassine_installer::release::Release;
use glassine_installer::validate::{Loaded, load};
use img::{MIB, Part};
use std::fs::{self, File};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const GIB: u64 = 1 << 30;

fn tree(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn put(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

/// A disk: its sysfs directory (behind a link like the kernel's, which
/// tells the transport), its partitions' dev numbers, and its image.
struct Disk<'a> {
    name: &'a str,
    link: &'a str,
    size: u64,
    removable: bool,
    major: u32,
    minor: u32,
    model: Option<&'a str>,
    parts: u32,
    writes: Vec<(u64, Vec<u8>)>,
}

fn add(root: &Path, d: &Disk) {
    let dev = format!("sys/{}", d.link);
    put(root, &format!("{dev}/size"), &format!("{}\n", d.size / 512));
    put(root, &format!("{dev}/removable"), if d.removable { "1\n" } else { "0\n" });
    put(root, &format!("{dev}/dev"), &format!("{}:{}\n", d.major, d.minor));
    put(root, &format!("{dev}/queue/logical_block_size"), "512\n");
    if let Some(m) = d.model {
        put(root, &format!("{dev}/device/model"), &format!("{m}  \n"));
    }
    // Never read: a serial next to the model.
    put(root, &format!("{dev}/device/serial"), "SERIAL-NEVER-READ\n");
    for i in 1..=d.parts {
        let pn = if d.name.starts_with("nvme") {
            format!("{}p{i}", d.name)
        } else {
            format!("{}{i}", d.name)
        };
        put(root, &format!("{dev}/{pn}/partition"), &format!("{i}\n"));
        put(root, &format!("{dev}/{pn}/dev"), &format!("{}:{}\n", d.major, d.minor + i));
    }
    fs::create_dir_all(root.join("sys/block")).unwrap();
    std::os::unix::fs::symlink(format!("../{}", d.link), root.join("sys/block").join(d.name)).unwrap();
    fs::create_dir_all(root.join("dev")).unwrap();
    let f = File::create(root.join("dev").join(d.name)).unwrap();
    f.set_len(d.size).unwrap();
    for (o, b) in &d.writes {
        f.write_all_at(b, *o).unwrap();
    }
}

fn at(base: u64, w: Vec<(u64, Vec<u8>)>) -> Vec<(u64, Vec<u8>)> {
    w.into_iter().map(|(o, b)| (base + o, b)).collect()
}

/// A PC with: a Windows-like NVMe disk, the installer's USB stick, a
/// virtio disk with another Linux, and a blank one too small for
/// GlassineOS.
fn machine(root: &Path) {
    let lba = |x: u64| x / 512;
    // disk nvme0n1: ESP, MSR, NTFS (signature), recovery (NTFS signature).
    let size = 64 * GIB;
    let mut w = img::gpt(
        size,
        512,
        &[
            Part {
                type_guid: img::ESP,
                first_lba: lba(MIB),
                last_lba: lba(101 * MIB) - 1,
                name: "EFI system partition",
            },
            Part {
                type_guid: img::MSR,
                first_lba: lba(101 * MIB),
                last_lba: lba(117 * MIB) - 1,
                name: "Microsoft reserved partition",
            },
            Part {
                type_guid: img::BASIC,
                first_lba: lba(117 * MIB),
                last_lba: lba(40 * GIB) - 1,
                name: "Basic data partition",
            },
            Part {
                type_guid: img::WINRE,
                first_lba: lba(40 * GIB),
                last_lba: lba(40 * GIB + 700 * MIB) - 1,
                name: "",
            },
        ],
    );
    w.extend(at(MIB, img::fat16(204800, 50).0));
    w.extend(at(117 * MIB, img::ntfs((40 * GIB - 117 * MIB) / 4096, 0, false)));
    w.extend(at(40 * GIB, img::ntfs(700 * MIB / 4096, 0, false)));
    add(
        root,
        &Disk {
            name: "nvme0n1",
            link: "devices/pci0000:00/0000:00:04.0/nvme/nvme0/nvme0n1",
            size,
            removable: false,
            major: 259,
            minor: 0,
            model: Some("Example NVMe"),
            parts: 4,
            writes: w,
        },
    );

    // disk sda: the installer's stick, one FAT partition.
    let size = 8 * GIB;
    let mut w = img::gpt(
        size,
        512,
        &[Part {
            type_guid: img::BASIC,
            first_lba: 2048,
            last_lba: lba(size) - 2048,
            name: "GLINSTALL",
        }],
    );
    w.extend(at(MIB, img::fat32(lba(size) as u32 - 4096, 10, None).0));
    add(
        root,
        &Disk {
            name: "sda",
            link: "devices/pci0000:00/0000:00:05.0/usb1/1-1/1-1:1.0/host0/target0:0:0/0:0:0:0/block/sda",
            size,
            removable: true,
            major: 8,
            minor: 0,
            model: Some("Example Stick"),
            parts: 1,
            writes: w,
        },
    );

    // disk vda: another Linux, ESP + ext4.
    let size = 32 * GIB;
    let mut w = img::gpt(
        size,
        512,
        &[
            Part {
                type_guid: img::ESP,
                first_lba: lba(MIB),
                last_lba: lba(513 * MIB) - 1,
                name: "",
            },
            Part {
                type_guid: img::LINUX,
                first_lba: lba(513 * MIB),
                last_lba: lba(20 * GIB) - 1,
                name: "",
            },
        ],
    );
    w.extend(at(MIB, img::fat32(lba(512 * MIB) as u32, 100, None).0));
    // 2 GiB used of the ext4.
    let blocks = (20 * GIB - 513 * MIB) / 4096;
    w.extend(at(513 * MIB, img::ext4(blocks, blocks - 2 * GIB / 4096, 2)));
    add(
        root,
        &Disk {
            name: "vda",
            link: "devices/pci0000:00/0000:00:06.0/virtio2/block/vda",
            size,
            removable: false,
            major: 253,
            minor: 0,
            model: None,
            parts: 2,
            writes: w,
        },
    );

    // disk vdb: blank, 4 GiB.
    add(
        root,
        &Disk {
            name: "vdb",
            link: "devices/pci0000:00/0000:00:07.0/virtio3/block/vdb",
            size: 4 * GIB,
            removable: false,
            major: 253,
            minor: 16,
            model: None,
            parts: 0,
            writes: vec![],
        },
    );

    // Skipped: a loop device and a CD drive.
    put(root, "sys/devices/virtual/block/loop0/size", "2048\n");
    std::os::unix::fs::symlink("../devices/virtual/block/loop0", root.join("sys/block/loop0")).unwrap();

    // The firmware.
    let g = glassine_installer::efivar::GLOBAL;
    let v = root.join("sys/firmware/efi/efivars");
    fs::create_dir_all(&v).unwrap();
    fs::write(
        v.join(format!("Boot0000-{g}")),
        img::load_option("UEFI Example Stick", None, "\\EFI\\BOOT\\BOOTX64.EFI"),
    )
    .unwrap();
    fs::write(
        v.join(format!("Boot0001-{g}")),
        img::load_option("Windows Boot Manager", None, "\\EFI\\Microsoft\\Boot\\bootmgfw.efi"),
    )
    .unwrap();
    fs::write(v.join(format!("BootOrder-{g}")), [7, 0, 0, 0, 1, 0, 0, 0]).unwrap();
    fs::write(v.join(format!("BootCurrent-{g}")), [6, 0, 0, 0, 0, 0]).unwrap();
    fs::write(v.join(format!("SecureBoot-{g}")), [6, 0, 0, 0, 0]).unwrap();
    // Some other vendor's variable: never read.
    fs::write(v.join("Secret-01234567-89ab-cdef-0123-456789abcdef"), [7, 0, 0, 0, 1, 2, 3]).unwrap();

    // The running system: root on a tmpfs, the stick mounted, the program on it.
    put(
        root,
        "proc/self/mountinfo",
        "22 1 0:21 / / rw,relatime - tmpfs rootfs rw\n\
         30 22 8:1 / /media/stick ro,relatime - vfat /dev/sda1 ro,fmask=0022\n\
         31 22 0:5 / /dev rw - devtmpfs devtmpfs rw\n",
    );
    std::os::unix::fs::symlink("/media/stick/glassine-install", root.join("proc/self/exe")).unwrap();
    put(root, "proc/sys/kernel/arch", "x86_64\n");
    put(root, "proc/cpuinfo", "processor\t: 0\nvendor_id\t: GenuineIntel\nmodel name\t: Example CPU\n");
    put(root, "sys/class/dmi/id/sys_vendor", "Example Computers\n");
    put(root, "sys/class/dmi/id/product_name", "Example 13\n");
    put(root, "sys/class/dmi/id/product_serial", "NEVER-READ\n");
}

fn pc_description() -> Description {
    let t = fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../devices/uefi-pc.toml")).unwrap();
    match load(&t) {
        Loaded::Valid(d) => *d,
        _ => panic!("devices/uefi-pc.toml doesn't load"),
    }
}

fn stamp(root: &Path) -> Vec<(PathBuf, u64, SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let m = fs::symlink_metadata(e.path()).unwrap();
            if m.is_dir() {
                stack.push(e.path());
            } else {
                out.push((e.path(), m.len(), m.modified().unwrap()));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn reads_a_pc_and_writes_nothing() {
    let root = tree("pc-tree");
    machine(&root);
    let before = stamp(&root);
    let got = glassine_installer_pc::inspect(&root).unwrap();
    assert_eq!(stamp(&root), before, "inspecting changed a file of the tree");
    let s = &got.snapshot;
    let json = serde_json::to_string(s).unwrap();
    assert!(!json.contains("NEVER-READ") && !json.contains("SERIAL"), "a serial reached the facts");

    let st = s.storage.as_ref().unwrap();
    let disks: Vec<(u32, &str, bool, Option<&str>)> = st.disks.iter().map(|d| (d.index, d.kind.as_str(), d.removable, d.table.as_deref())).collect();
    assert_eq!(
        disks,
        vec![
            (0, "nvme", false, Some("gpt")),
            (1, "usb", true, Some("gpt")),
            (2, "virtio", false, Some("gpt")),
            (3, "virtio", false, None)
        ]
    );
    assert_eq!(st.disks[0].model.as_deref(), Some("Example NVMe"));
    let i = st.installer_disk.as_ref().unwrap();
    assert_eq!((i.index, i.by.as_str()), (1, "executable"));

    let fs0: Vec<Option<&str>> = st.disks[0].partitions.iter().map(|p| p.fs.as_ref().map(|f| f.kind.as_str())).collect();
    assert_eq!(fs0, vec![Some("vfat"), None, Some("ntfs"), Some("ntfs")]);
    let ext = st.disks[2].partitions[1].fs.as_ref().unwrap();
    assert_eq!((ext.kind.as_str(), ext.used), ("ext4", Some(2 * GIB)));
    assert_eq!(st.disks[3].free, vec![4 * GIB - 2 * MIB]);

    let sys: Vec<(&str, u32, Vec<u32>)> = s.systems.iter().map(|x| (x.kind.as_str(), x.disk, x.partitions.clone())).collect();
    assert_eq!(sys, vec![("windows", 0, vec![2, 3, 4]), ("linux", 2, vec![2])]);

    let f = s.firmware.as_ref().unwrap();
    assert_eq!(
        (f.kind.as_str(), f.secure_boot.as_deref(), f.variables.as_deref()),
        ("uefi", Some("off"), Some("readable"))
    );
    assert_eq!(f.boot_entries.len(), 2);
    assert_eq!(f.boot_order, vec!["0001", "0000"]);
    assert_eq!(s.cpu.as_ref().unwrap().arch, "x86-64");
    assert_eq!(s.board.as_ref().unwrap().product.as_deref(), Some("Example 13"));
    assert!(s.board.as_ref().unwrap().serial.is_none());
}

#[test]
fn the_plan_on_what_it_read() {
    let root = tree("pc-tree-plan");
    machine(&root);
    let s = glassine_installer_pc::inspect(&root).unwrap().snapshot;
    let d = pc_description();
    let ds = [&d];
    let rec = recognize(&ds, &s);
    let run = |choices: &[(&str, &str)]| {
        let mut i = Input {
            developer: true,
            // The release states the system partition's least size: 8 GiB.
            release: Some(Release::parse(include_str!("../../core/tests/fixtures/releases/base-8gib.json")).unwrap()),
            ..Default::default()
        };
        for (k, v) in choices {
            i.choices.insert(k.to_string(), v.to_string());
        }
        plan::plan(&ds, &s, &rec, &i)
    };

    // The general PC description fits; by default GlassineOS goes on the
    // first fixed disk besides the installer's.
    let p = run(&[]);
    assert_eq!(p.description, "uefi-pc");
    assert_eq!(p.status, Status::Ready, "{:?}", p.reasons);
    assert_eq!(p.space.disk.as_ref().unwrap().disk, 0);
    assert_eq!(p.systems.len(), 2);

    // The installer's own disk: never a target.
    let p = run(&[("target-disk", "disk1")]);
    assert_eq!(p.status, Status::Refused);
    assert!(
        p.reasons.iter().any(|r| r.contains("disk 1 is the disk the installer runs from")),
        "{:?}",
        p.reasons
    );

    // Too small: refused with what it lacks.
    let p = run(&[("target-disk", "disk3")]);
    assert_eq!(p.status, Status::Refused);
    let sp = p.space.disk.as_ref().unwrap();
    assert_eq!((sp.disk, sp.needs, sp.free), (3, 9 * GIB, 4 * GIB - 2 * MIB));
    assert_eq!(sp.lacks, 5 * GIB + 2 * MIB);
    assert!(
        p.reasons.iter().any(|r| r.contains("disk 3 is too small") && r.contains("lacks")),
        "{:?}",
        p.reasons
    );
    // Without the release that least isn't known yet: not refused for it.
    let mut i = Input {
        developer: true,
        ..Default::default()
    };
    i.choices.insert("target-disk".into(), "disk3".into());
    let p = plan::plan(&ds, &s, &rec, &i);
    assert_eq!(p.status, Status::Ready, "{:?}", p.reasons);
    let sp = p.space.disk.as_ref().unwrap();
    assert_eq!((sp.needs, sp.lacks, sp.exact), (GIB, 0, false));

    // The other Linux's disk: its image is at most the disk less what its
    // file systems report free.
    let p = run(&[("target-disk", "disk2")]);
    assert_eq!(p.status, Status::Ready, "{:?}", p.reasons);
    let img = &p.backup.images[0];
    assert_eq!(img.estimate, "at-most");
    let disk2 = &s.storage.as_ref().unwrap().disks[2];
    let unused: u64 = disk2.partitions.iter().filter_map(|x| x.fs.as_ref()?.used.map(|u| x.size - u)).sum();
    assert!(unused > 17 * GIB, "{unused}");
    assert_eq!(img.estimate_bytes, Some(32 * GIB - unused));

    // Replacing the system on the installer's disk can't happen: it holds
    // none. The backup to the installer's media while writing to it: refused
    // (here by also choosing it as the target).
    let p = run(&[("target-disk", "disk1"), ("backup-to", "installer-media")]);
    assert!(p.reasons.iter().any(|r| r.contains("installer's own disk 1")), "{:?}", p.reasons);
}

#[test]
fn without_knowing_the_installers_disk_a_removable_target_is_refused_for_the_backup() {
    let root = tree("pc-tree-unknown");
    machine(&root);
    // No program link, root on a tmpfs, the current boot entry has no disk.
    fs::remove_file(root.join("proc/self/exe")).unwrap();
    let s = glassine_installer_pc::inspect(&root).unwrap();
    assert!(s.snapshot.storage.as_ref().unwrap().installer_disk.is_none());
    assert!(s.notes.iter().any(|n| n.contains("couldn't tell")));
    let d = pc_description();
    let ds = [&d];
    let rec = recognize(&ds, &s.snapshot);
    let mut i = Input {
        developer: true,
        ..Default::default()
    };
    i.choices.insert("target-disk".into(), "disk1".into());
    let p = plan::plan(&ds, &s.snapshot, &rec, &i);
    assert!(
        p.reasons.iter().any(|r| r.contains("couldn't tell which disk the installer runs from")),
        "{:?}",
        p.reasons
    );
}

#[test]
fn a_disk_it_may_not_read_is_unreadable_and_never_written() {
    let root = tree("pc-tree-denied");
    machine(&root);
    // The NVMe image is gone: the device can't be opened.
    fs::remove_file(root.join("dev/nvme0n1")).unwrap();
    let got = glassine_installer_pc::inspect(&root).unwrap();
    let st = got.snapshot.storage.as_ref().unwrap();
    assert_eq!(st.disks[0].table.as_deref(), Some("unreadable"));
    assert!(got.notes.iter().any(|n| n.starts_with("disk nvme0n1:")), "{:?}", got.notes);
    assert!(got.notes.iter().all(|n| !n.contains(root.to_str().unwrap())), "a note names the tree's path");
    let d = pc_description();
    let ds = [&d];
    let rec = recognize(&ds, &got.snapshot);
    let mut i = Input {
        developer: true,
        ..Default::default()
    };
    i.choices.insert("target-disk".into(), "disk0".into());
    let p = plan::plan(&ds, &got.snapshot, &rec, &i);
    assert_eq!(p.status, Status::Refused);
    assert!(p.reasons.iter().any(|r| r.contains("can't be read")), "{:?}", p.reasons);
}

#[test]
fn the_boot_entry_tells_the_disk_when_nothing_else_does() {
    let root = tree("pc-tree-bootentry");
    machine(&root);
    fs::remove_file(root.join("proc/self/exe")).unwrap();
    // The current entry starts from the stick's partition (its unique GUID
    // as the test images make it: the disk's size in MiB, then the slot).
    let g = glassine_installer::efivar::GLOBAL;
    let v = root.join("sys/firmware/efi/efivars");
    fs::write(
        v.join(format!("Boot0000-{g}")),
        img::load_option("UEFI Example Stick", Some("00002000-0000-4000-8000-000000000001"), "\\EFI\\BOOT\\BOOTX64.EFI"),
    )
    .unwrap();
    let got = glassine_installer_pc::inspect(&root).unwrap().snapshot;
    let i = got.storage.as_ref().unwrap().installer_disk.as_ref().unwrap();
    assert_eq!((i.index, i.by.as_str()), (1, "boot-entry"));
}
