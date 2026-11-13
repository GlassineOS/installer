// SPDX-License-Identifier: GPL-2.0-or-later
//! The PC inspect's parsers on small images made here: GPT and MBR, the
//! file systems' superblocks and used bytes, the EFI boot entries.

mod common;
use common::img::{self, Part};
use glassine_installer::efivar;
use glassine_installer::fsprobe::probe;
use glassine_installer::pcfacts::{self, DiskInfo};
use glassine_installer::table::{self, Kind, MIB};

fn disk(size: u64, writes: &[(u64, Vec<u8>)]) -> Vec<u8> {
    let mut d = vec![0u8; size as usize];
    img::put(&mut d, 0, writes);
    d
}

fn parts() -> Vec<Part<'static>> {
    vec![
        Part {
            type_guid: img::ESP,
            first_lba: 2048,
            last_lba: 2048 + 8191,
            name: "EFI system partition",
        },
        Part {
            type_guid: img::LINUX,
            first_lba: 12288,
            last_lba: 12288 + 20479,
            name: "root",
        },
    ]
}

#[test]
fn gpt_is_read_with_names_types_and_free_space() {
    let size = 64 * MIB;
    let d = disk(size, &img::gpt(size, 512, &parts()));
    let t = table::read(&mut d.as_slice(), size, 512).unwrap();
    assert_eq!(t.kind, Kind::Gpt);
    assert!(t.primary_ok);
    assert_eq!(t.entries.len(), 2);
    assert_eq!(t.entries[0].index, 1);
    assert_eq!(t.entries[0].type_id, img::ESP);
    assert_eq!(t.entries[0].start, MIB);
    assert_eq!(t.entries[0].size, 4 * MIB);
    assert_eq!(t.entries[1].name.as_deref(), Some("root"));
    assert_eq!(t.entries[1].guid.as_deref(), Some("00000040-0000-4000-8000-000000000002"));
    // [5 MiB, 6 MiB) between them, and from 16 MiB to the last usable LBA
    // (rounded down to MiB: 63 MiB).
    assert_eq!(t.free, vec![(5 * MIB, MIB), (16 * MIB, 47 * MIB)]);
}

#[test]
fn a_damaged_primary_gpt_falls_back_to_the_backup() {
    let size = 64 * MIB;
    let mut d = disk(size, &img::gpt(size, 512, &parts()));
    d[512 + 40] ^= 0xff; // the primary header's CRC no longer matches
    let t = table::read(&mut d.as_slice(), size, 512).unwrap();
    assert_eq!(t.kind, Kind::Gpt);
    assert!(!t.primary_ok);
    assert_eq!(t.entries.len(), 2);
}

#[test]
fn both_gpt_copies_damaged_is_unreadable_not_blank() {
    let size = 64 * MIB;
    let mut d = disk(size, &img::gpt(size, 512, &parts()));
    d[512 + 40] ^= 0xff;
    let last = (size - 512) as usize;
    d[last + 40] ^= 0xff;
    let t = table::read(&mut d.as_slice(), size, 512).unwrap();
    assert_eq!(t.kind, Kind::Unreadable);
    assert!(t.free.is_empty(), "an unreadable table offers no free space");
}

#[test]
fn damaged_entries_are_caught_by_their_crc() {
    let size = 64 * MIB;
    let mut d = disk(size, &img::gpt(size, 512, &parts()));
    d[1024 + 33] ^= 1; // a partition's first LBA in the primary array
    let t = table::read(&mut d.as_slice(), size, 512).unwrap();
    // The backup's array is intact.
    assert_eq!((t.kind, t.primary_ok), (Kind::Gpt, false));
    assert_eq!(t.entries[0].start, MIB);
}

#[test]
fn gpt_on_4k_sectors() {
    let size = 64 * MIB;
    let p = [Part {
        type_guid: img::LINUX,
        first_lba: 256,
        last_lba: 256 + 2559,
        name: "data",
    }];
    let d = disk(size, &img::gpt(size, 4096, &p));
    // The kernel's sector size unknown: 512 is tried first, then 4096.
    let t = table::read(&mut d.as_slice(), size, 0).unwrap();
    assert_eq!((t.kind, t.sector), (Kind::Gpt, 4096));
    assert_eq!(t.entries[0].start, MIB);
    assert_eq!(t.entries[0].size, 10 * MIB);
}

#[test]
fn mbr_with_logical_partitions() {
    let size = 64 * MIB;
    let mut d = disk(size, &[(0, img::mbr(&[(0x0c, 2048, 8192), (0x05, 10240, 40960)]))]);
    // Two logical partitions in the extended one.
    let ext = 10240u64;
    img::put(&mut d, ext * 512, &[(0, img::ebr(0x83, 2048, 10240, 14336, 12288))]);
    img::put(&mut d, (ext + 14336) * 512, &[(0, img::ebr(0x82, 2048, 8192, 0, 0))]);
    let t = table::read(&mut d.as_slice(), size, 512).unwrap();
    assert_eq!(t.kind, Kind::Mbr);
    let got: Vec<(u32, &str, u64, u64)> = t.entries.iter().map(|e| (e.index, e.type_id.as_str(), e.start / 512, e.size / 512)).collect();
    assert_eq!(
        got,
        vec![
            (1, "0x0c", 2048, 8192),
            (2, "0x05", 10240, 40960),
            (5, "0x83", 12288, 10240),
            (6, "0x82", 26624, 8192),
        ]
    );
    // Free: after the extended partition (ends at 25 MiB) to the end.
    assert_eq!(t.free, vec![(25 * MIB, 39 * MIB)]);
}

#[test]
fn a_blank_disk_has_no_table_and_is_all_free() {
    let size = 64 * MIB;
    let d = disk(size, &[]);
    let t = table::read(&mut d.as_slice(), size, 512).unwrap();
    assert_eq!(t.kind, Kind::None);
    assert_eq!(t.free, vec![(MIB, 62 * MIB)]);
}

#[test]
fn a_file_system_on_the_whole_disk_is_no_free_space() {
    let size = 64 * MIB;
    let (w, _) = img::fat16(size as u32 / 512, 10);
    let d = disk(size, &w);
    let l = pcfacts::layout(&mut d.as_slice(), size, 512).unwrap();
    assert_eq!(l.table.kind, Kind::None, "a FAT boot sector isn't an MBR");
    assert_eq!(l.whole.as_ref().map(|p| p.kind.as_str()), Some("vfat"));
    assert!(l.table.free.is_empty());
    let f = pcfacts::disk(0, &DiskInfo { size, ..Default::default() }, Some(&l));
    assert_eq!(f.partitions.len(), 1);
    assert_eq!(f.partitions[0].index, 0);
}

#[test]
fn ext4_reports_used_bytes() {
    let size = 64 * MIB;
    let d = disk(size, &img::ext4(16384, 10000, 2));
    let p = probe(&mut d.as_slice(), 0, size).unwrap();
    assert_eq!(p.kind, "ext4");
    assert_eq!(p.used, Some(6384 * 4096));
    let d = disk(size, &img::ext2(65536, 60000));
    let p = probe(&mut d.as_slice(), 0, size).unwrap();
    assert_eq!((p.kind.as_str(), p.used), ("ext2", Some(5536 * 1024)));
}

#[test]
fn ext4_with_more_free_than_blocks_tells_nothing() {
    let size = 64 * MIB;
    let d = disk(size, &img::ext4(100, 200, 2));
    assert_eq!(probe(&mut d.as_slice(), 0, size).unwrap().used, None);
}

#[test]
fn btrfs_and_xfs_report_used_bytes() {
    let size = 64 * MIB;
    let d = disk(size, &img::btrfs(size, 3 * MIB));
    let p = probe(&mut d.as_slice(), 0, size).unwrap();
    assert_eq!((p.kind.as_str(), p.used), ("btrfs", Some(3 * MIB)));
    let d = disk(size, &img::xfs(4096, 16384, 16000));
    let p = probe(&mut d.as_slice(), 0, size).unwrap();
    assert_eq!((p.kind.as_str(), p.used), ("xfs", Some(384 * 4096)));
}

#[test]
fn fat16_counts_its_fat() {
    let size = 64 * MIB;
    let (w, used) = img::fat16(size as u32 / 512, 123);
    let d = disk(size, &w);
    let p = probe(&mut d.as_slice(), 0, size).unwrap();
    assert_eq!((p.kind.as_str(), p.used), ("vfat", Some(used)));
}

#[test]
fn fat32_uses_fsinfo_and_else_its_fat() {
    let size = 300 * MIB;
    let sectors = size as u32 / 512;
    let (w, used) = img::fat32(sectors, 77, None);
    let d = disk(size, &w);
    assert_eq!(probe(&mut d.as_slice(), 0, size).unwrap().used, Some(used));
    // FSInfo says it doesn't keep the count: the FAT is counted instead.
    let (w, used) = img::fat32(sectors, 77, Some(0xffff_ffff));
    let d = disk(size, &w);
    assert_eq!(probe(&mut d.as_slice(), 0, size).unwrap().used, Some(used));
}

#[test]
fn ntfs_counts_its_cluster_bitmap() {
    let clusters = 33_000u64; // 4125 bitmap bytes: two runs
    let size = clusters * 4096;
    let d = disk(size, &img::ntfs(clusters, 1234, true));
    let p = probe(&mut d.as_slice(), 0, size).unwrap();
    assert_eq!((p.kind.as_str(), p.used), ("ntfs", Some(1234 * 4096)));
}

#[test]
fn ntfs_signature_only_has_no_used_bytes() {
    let size = 64 * MIB;
    let d = disk(size, &img::ntfs(size / 4096, 0, false));
    let p = probe(&mut d.as_slice(), 0, size).unwrap();
    assert_eq!((p.kind.as_str(), p.used), ("ntfs", None));
}

#[test]
fn swap_luks_and_nothing() {
    let size = 64 * MIB;
    let d = disk(size, &img::swap());
    assert_eq!(probe(&mut d.as_slice(), 0, size).unwrap().kind, "swap");
    let d = disk(size, &img::luks());
    assert_eq!(probe(&mut d.as_slice(), 0, size).unwrap().kind, "crypto_LUKS");
    let d = disk(size, &[]);
    assert_eq!(probe(&mut d.as_slice(), 0, size), None);
}

#[test]
fn a_probe_never_reads_past_its_partition() {
    // A btrfs superblock just past a 64-KiB partition: not this partition's.
    let size = 4 * MIB;
    let d = disk(size, &img::btrfs(size, MIB));
    assert_eq!(probe(&mut d.as_slice(), 0, 0x10000), None);
}

#[test]
fn file_systems_on_a_gpt_disk_and_the_systems_they_make() {
    let size = 256 * MIB;
    let p = [
        Part {
            type_guid: img::ESP,
            first_lba: 2048,
            last_lba: 2048 + 65535,
            name: "EFI",
        },
        Part {
            type_guid: img::MSR,
            first_lba: 67584,
            last_lba: 67584 + 32767,
            name: "Microsoft reserved partition",
        },
        Part {
            type_guid: img::BASIC,
            first_lba: 100352,
            last_lba: 100352 + 204799,
            name: "Basic data partition",
        },
    ];
    let mut d = disk(size, &img::gpt(size, 512, &p));
    let (fat, _) = img::fat16(65536, 3);
    img::put(&mut d, 2048 * 512, &fat);
    img::put(&mut d, 100352 * 512, &img::ntfs(204800 / 8, 0, false));
    let l = pcfacts::layout(&mut d.as_slice(), size, 512).unwrap();
    let f = pcfacts::disk(
        0,
        &DiskInfo {
            kind: "nvme".into(),
            size,
            ..Default::default()
        },
        Some(&l),
    );
    let fs: Vec<Option<&str>> = f.partitions.iter().map(|p| p.fs.as_ref().map(|f| f.kind.as_str())).collect();
    assert_eq!(fs, vec![Some("vfat"), None, Some("ntfs")]);
    let s = pcfacts::systems(std::slice::from_ref(&f), None);
    assert_eq!(s.len(), 1);
    assert_eq!((s[0].kind.as_str(), s[0].partitions.clone()), ("windows", vec![2, 3]));
    // The installer's own disk holds no system.
    assert!(pcfacts::systems(&[f], Some(0)).is_empty());
}

#[test]
fn boot_entries_and_secure_boot_from_efivars() {
    let g = efivar::GLOBAL;
    let vars = pcfacts::EfiVars {
        store: "readable".into(),
        vars: vec![
            (
                format!("Boot0001-{g}"),
                img::load_option("Linux Boot Manager", Some(img::ESP), "\\EFI\\systemd\\systemd-bootx64.efi"),
            ),
            (format!("Boot0000-{g}"), img::load_option("UEFI Shell", None, "\\shell.efi")),
            (format!("BootOrder-{g}"), vec![7, 0, 0, 0, 1, 0, 0, 0]),
            (format!("BootCurrent-{g}"), vec![7, 0, 0, 0, 1, 0]),
            (format!("SecureBoot-{g}"), vec![6, 0, 0, 0, 1]),
            (format!("SetupMode-{g}"), vec![6, 0, 0, 0, 0]),
        ],
    };
    let (f, opts) = pcfacts::firmware(true, &vars);
    assert_eq!(f.kind, "uefi");
    assert_eq!(f.secure_boot.as_deref(), Some("on"));
    assert_eq!(f.boot_order, vec!["0001", "0000"]);
    assert_eq!(f.boot_current.as_deref(), Some("0001"));
    let nums: Vec<&str> = f.boot_entries.iter().map(|e| e.number.as_str()).collect();
    assert_eq!(nums, vec!["0000", "0001"]);
    assert_eq!(f.boot_entries[1].label.as_deref(), Some("Linux Boot Manager"));
    assert_eq!(f.boot_entries[1].path.as_deref(), Some("\\EFI\\systemd\\systemd-bootx64.efi"));
    assert_eq!(opts[1].partition_guid.as_deref(), Some(img::ESP));
    assert_eq!(opts[0].partition_guid, None);

    let (f, _) = pcfacts::firmware(
        true,
        &pcfacts::EfiVars {
            store: "not-mounted".into(),
            vars: vec![],
        },
    );
    assert_eq!((f.secure_boot, f.variables.as_deref()), (None, Some("not-mounted")));
    let (f, _) = pcfacts::firmware(false, &pcfacts::EfiVars::default());
    assert_eq!(f.kind, "bios");
}
