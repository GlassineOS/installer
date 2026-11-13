// SPDX-License-Identifier: GPL-2.0-or-later
//! Small disk images made in the tests: partition tables and superblocks
//! written by hand, so the parsers are tested on bytes no tool of the host
//! made. Shared with the PC reader's tests (`#[path]`).
#![allow(dead_code)]

use glassine_installer::table::crc32;

pub const MIB: u64 = 1 << 20;
pub const ESP: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
pub const LINUX: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";
pub const MSR: &str = "e3c9e316-0b5c-4db8-817d-f92df00215ae";
pub const BASIC: &str = "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7";
pub const WINRE: &str = "de94bba4-06d1-4d40-a16a-bfd50179d6ac";

/// The on-disk (mixed-endian) bytes of a GUID in text form.
pub fn guid_bytes(g: &str) -> [u8; 16] {
    let h: String = g.chars().filter(|c| *c != '-').collect();
    let b: Vec<u8> = (0..16).map(|i| u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap()).collect();
    let mut o = [0u8; 16];
    o[0..4].copy_from_slice(&[b[3], b[2], b[1], b[0]]);
    o[4..6].copy_from_slice(&[b[5], b[4]]);
    o[6..8].copy_from_slice(&[b[7], b[6]]);
    o[8..16].copy_from_slice(&b[8..16]);
    o
}

pub struct Part<'a> {
    pub type_guid: &'a str,
    pub first_lba: u64,
    pub last_lba: u64,
    pub name: &'a str,
}

/// The writes that put a GPT (protective MBR, both headers, both entry
/// arrays) on a disk of `size` bytes with `sector`-byte sectors, as
/// (offset, bytes).
pub fn gpt(size: u64, sector: u64, parts: &[Part]) -> Vec<(u64, Vec<u8>)> {
    let lbas = size / sector;
    let n = 128u64;
    let esz = 128u64;
    let entry_lbas = (n * esz).div_ceil(sector);
    let first = 2 + entry_lbas;
    let last = lbas - 1 - entry_lbas - 1;
    let mut entries = vec![0u8; (n * esz) as usize];
    for (i, p) in parts.iter().enumerate() {
        let e = &mut entries[i * esz as usize..(i + 1) * esz as usize];
        e[0..16].copy_from_slice(&guid_bytes(p.type_guid));
        // Unique per disk size and slot: `<size in MiB>-0000-4000-8000-<slot>`.
        e[16..32].copy_from_slice(&guid_bytes(&format!("{:08x}-0000-4000-8000-{:012x}", size >> 20, i + 1)));
        e[32..40].copy_from_slice(&p.first_lba.to_le_bytes());
        e[40..48].copy_from_slice(&p.last_lba.to_le_bytes());
        for (j, u) in p.name.encode_utf16().enumerate() {
            e[56 + j * 2..58 + j * 2].copy_from_slice(&u.to_le_bytes());
        }
    }
    let ecrc = crc32(&entries);
    let header = |my: u64, alt: u64, elba: u64| {
        let mut h = vec![0u8; sector as usize];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&my.to_le_bytes());
        h[32..40].copy_from_slice(&alt.to_le_bytes());
        h[40..48].copy_from_slice(&first.to_le_bytes());
        h[48..56].copy_from_slice(&last.to_le_bytes());
        h[56..72].copy_from_slice(&guid_bytes("11111111-2222-4333-8444-555555555555"));
        h[72..80].copy_from_slice(&elba.to_le_bytes());
        h[80..84].copy_from_slice(&(n as u32).to_le_bytes());
        h[84..88].copy_from_slice(&(esz as u32).to_le_bytes());
        h[88..92].copy_from_slice(&ecrc.to_le_bytes());
        let c = crc32(&h[0..92]);
        h[16..20].copy_from_slice(&c.to_le_bytes());
        h
    };
    let mut mbr = vec![0u8; 512];
    mbr[446 + 4] = 0xee;
    mbr[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
    mbr[446 + 12..446 + 16].copy_from_slice(&((lbas - 1).min(u32::MAX as u64) as u32).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xaa;
    vec![
        (0, mbr),
        (sector, header(1, lbas - 1, 2)),
        (2 * sector, entries.clone()),
        ((lbas - 1 - entry_lbas) * sector, entries),
        ((lbas - 1) * sector, header(lbas - 1, 1, lbas - 1 - entry_lbas)),
    ]
}

/// An MBR with these (type, first sector, sectors) primary entries.
pub fn mbr(parts: &[(u8, u32, u32)]) -> Vec<u8> {
    let mut b = vec![0u8; 512];
    for (i, (t, s, n)) in parts.iter().enumerate() {
        let e = 446 + i * 16;
        b[e + 4] = *t;
        b[e + 8..e + 12].copy_from_slice(&s.to_le_bytes());
        b[e + 12..e + 16].copy_from_slice(&n.to_le_bytes());
    }
    b[510] = 0x55;
    b[511] = 0xaa;
    b
}

/// An extended boot record: one logical partition (relative to this
/// record) and the link to the next record (relative to the extended
/// partition's start), 0 for none.
pub fn ebr(ty: u8, rel: u32, len: u32, next: u32, next_len: u32) -> Vec<u8> {
    let mut b = mbr(&[(ty, rel, len)]);
    if next != 0 {
        let e = 446 + 16;
        b[e + 4] = 0x05;
        b[e + 8..e + 12].copy_from_slice(&next.to_le_bytes());
        b[e + 12..e + 16].copy_from_slice(&next_len.to_le_bytes());
    }
    b
}

/// An ext4 superblock (64-bit) at offset 1024 of a partition.
pub fn ext4(blocks: u64, free: u64, log_block: u32) -> Vec<(u64, Vec<u8>)> {
    let mut sb = vec![0u8; 1024];
    sb[0x04..0x08].copy_from_slice(&(blocks as u32).to_le_bytes());
    sb[0x0c..0x10].copy_from_slice(&(free as u32).to_le_bytes());
    sb[0x18..0x1c].copy_from_slice(&log_block.to_le_bytes());
    sb[0x38..0x3a].copy_from_slice(&0xef53u16.to_le_bytes());
    sb[0x5c..0x60].copy_from_slice(&4u32.to_le_bytes()); // has_journal
    sb[0x60..0x64].copy_from_slice(&(0x40u32 | 0x80 | 0x200).to_le_bytes()); // extents, 64bit, flex_bg
    sb[0x150..0x154].copy_from_slice(&((blocks >> 32) as u32).to_le_bytes());
    sb[0x158..0x15c].copy_from_slice(&((free >> 32) as u32).to_le_bytes());
    vec![(1024, sb)]
}

/// An ext2 superblock: no journal, no ext4 features.
pub fn ext2(blocks: u32, free: u32) -> Vec<(u64, Vec<u8>)> {
    let mut sb = vec![0u8; 1024];
    sb[0x04..0x08].copy_from_slice(&blocks.to_le_bytes());
    sb[0x0c..0x10].copy_from_slice(&free.to_le_bytes());
    sb[0x38..0x3a].copy_from_slice(&0xef53u16.to_le_bytes());
    vec![(1024, sb)]
}

pub fn btrfs(total: u64, used: u64) -> Vec<(u64, Vec<u8>)> {
    let mut sb = vec![0u8; 4096];
    sb[0x40..0x48].copy_from_slice(b"_BHRfS_M");
    sb[0x70..0x78].copy_from_slice(&total.to_le_bytes());
    sb[0x78..0x80].copy_from_slice(&used.to_le_bytes());
    vec![(0x10000, sb)]
}

pub fn xfs(block: u32, blocks: u64, free: u64) -> Vec<(u64, Vec<u8>)> {
    let mut sb = vec![0u8; 512];
    sb[0..4].copy_from_slice(b"XFSB");
    sb[4..8].copy_from_slice(&block.to_be_bytes());
    sb[8..16].copy_from_slice(&blocks.to_be_bytes());
    sb[0x90..0x98].copy_from_slice(&free.to_be_bytes());
    vec![(0, sb)]
}

pub fn swap() -> Vec<(u64, Vec<u8>)> {
    vec![(4086, b"SWAPSPACE2".to_vec())]
}

pub fn luks() -> Vec<(u64, Vec<u8>)> {
    vec![(0, b"LUKS\xba\xbe\x00\x02".to_vec())]
}

fn bpb(bps: u16, spc: u8, reserved: u16, nfats: u8, root_entries: u16, total: u32, fatsz16: u16) -> Vec<u8> {
    let mut b = vec![0u8; 512];
    b[0] = 0xeb;
    b[1] = 0x3c;
    b[2] = 0x90;
    b[3..11].copy_from_slice(b"mkfs.fat");
    b[11..13].copy_from_slice(&bps.to_le_bytes());
    b[13] = spc;
    b[14..16].copy_from_slice(&reserved.to_le_bytes());
    b[16] = nfats;
    b[17..19].copy_from_slice(&root_entries.to_le_bytes());
    if total < 65536 {
        b[19..21].copy_from_slice(&(total as u16).to_le_bytes());
    } else {
        b[32..36].copy_from_slice(&total.to_le_bytes());
    }
    b[22..24].copy_from_slice(&fatsz16.to_le_bytes());
    b[510] = 0x55;
    b[511] = 0xaa;
    b
}

/// A FAT16 volume of `total` 512-byte sectors, 4 sectors a cluster, with the
/// first `used` clusters allocated in its FATs. Returns the writes and the
/// bytes it should report used.
pub fn fat16(total: u32, used: u32) -> (Vec<(u64, Vec<u8>)>, u64) {
    let (reserved, nfats, root, spc) = (4u16, 2u8, 512u16, 4u8);
    let clusters_guess = total / spc as u32;
    let fatsz = ((clusters_guess + 2) * 2).div_ceil(512) as u16;
    let b = bpb(512, spc, reserved, nfats, root, total, fatsz);
    let mut fat = vec![0u8; fatsz as usize * 512];
    fat[0..4].copy_from_slice(&[0xf8, 0xff, 0xff, 0xff]);
    for c in 2..used + 2 {
        fat[c as usize * 2..c as usize * 2 + 2].copy_from_slice(&0xffffu16.to_le_bytes());
    }
    let meta = reserved as u64 + nfats as u64 * fatsz as u64 + (root as u64 * 32).div_ceil(512);
    let mut w = vec![(0, b), (reserved as u64 * 512, fat.clone())];
    w.push(((reserved as u64 + fatsz as u64) * 512, fat));
    (w, (meta + used as u64 * spc as u64) * 512)
}

/// A FAT32 volume of `total` 512-byte sectors, 8 sectors a cluster, with
/// `used` clusters allocated, and an FSInfo free count (or 0xffffffff: not
/// kept). Returns the writes and the bytes it should report used.
pub fn fat32(total: u32, used: u32, fsinfo_free: Option<u32>) -> (Vec<(u64, Vec<u8>)>, u64) {
    let (reserved, nfats, spc) = (32u16, 2u8, 8u8);
    let fatsz = ((total / spc as u32 + 2) * 4).div_ceil(512);
    let mut b = bpb(512, spc, reserved, nfats, 0, total, 0);
    b[36..40].copy_from_slice(&fatsz.to_le_bytes());
    b[44..48].copy_from_slice(&2u32.to_le_bytes());
    b[48..50].copy_from_slice(&1u16.to_le_bytes());
    let meta = reserved as u64 + nfats as u64 * fatsz as u64;
    let clusters = (total as u64 - meta) / spc as u64;
    let mut fi = vec![0u8; 512];
    fi[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
    fi[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
    let free = fsinfo_free.unwrap_or((clusters - used as u64) as u32);
    fi[488..492].copy_from_slice(&free.to_le_bytes());
    fi[510] = 0x55;
    fi[511] = 0xaa;
    let mut fat = vec![0u8; fatsz as usize * 512];
    for c in 0..used + 2 {
        fat[c as usize * 4..c as usize * 4 + 4].copy_from_slice(&0x0fff_ffffu32.to_le_bytes());
    }
    (vec![(0, b), (512, fi), (reserved as u64 * 512, fat)], (meta + used as u64 * spc as u64) * 512)
}

/// An NTFS volume of `clusters` 4-KiB clusters with `used` of them marked in
/// `$Bitmap` (MFT record 6, its $DATA in two runs to test the run list).
/// `with_mft` false gives the boot sector only (a signature).
pub fn ntfs(clusters: u64, used: u64, with_mft: bool) -> Vec<(u64, Vec<u8>)> {
    let cluster = 4096u64;
    let mut b = vec![0u8; 512];
    b[0..3].copy_from_slice(&[0xeb, 0x52, 0x90]);
    b[3..11].copy_from_slice(b"NTFS    ");
    b[11..13].copy_from_slice(&512u16.to_le_bytes());
    b[13] = 8;
    b[0x28..0x30].copy_from_slice(&(clusters * 8).to_le_bytes());
    let mft_lcn = 4u64;
    b[0x30..0x38].copy_from_slice(&mft_lcn.to_le_bytes());
    b[0x40] = (-10i8) as u8; // 1024-byte records
    b[510] = 0x55;
    b[511] = 0xaa;
    let mut w = vec![(0, b)];
    if !with_mft {
        return w;
    }
    let bitmap_len = clusters.div_ceil(8);
    let mut bitmap = vec![0u8; bitmap_len as usize];
    for c in 0..used {
        bitmap[(c / 8) as usize] |= 1 << (c % 8);
    }
    // Two runs: the first cluster at LCN 20, the rest at LCN 40.
    let first_len = 1u64;
    let rest = bitmap_len.div_ceil(cluster).saturating_sub(1).max(1);
    let mut rec = vec![0u8; 1024];
    rec[0..4].copy_from_slice(b"FILE");
    rec[4..6].copy_from_slice(&0x30u16.to_le_bytes()); // update sequence array at 0x30
    rec[6..8].copy_from_slice(&3u16.to_le_bytes()); // 1 + 2 sectors
    rec[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes()); // first attribute
    let usn = [0x07u8, 0x00];
    rec[0x30..0x32].copy_from_slice(&usn);
    // The attribute: non-resident $DATA, run list at 0x40 of it.
    let a = 0x38usize;
    let runs: Vec<u8> = vec![0x11, first_len as u8, 20, 0x11, rest as u8, 20, 0x00];
    let alen = (0x40 + runs.len()).div_ceil(8) * 8;
    rec[a..a + 4].copy_from_slice(&0x80u32.to_le_bytes());
    rec[a + 4..a + 8].copy_from_slice(&(alen as u32).to_le_bytes());
    rec[a + 8] = 1;
    rec[a + 0x20..a + 0x22].copy_from_slice(&0x40u16.to_le_bytes());
    rec[a + 0x30..a + 0x38].copy_from_slice(&bitmap_len.to_le_bytes());
    rec[a + 0x40..a + 0x40 + runs.len()].copy_from_slice(&runs);
    rec[a + alen..a + alen + 4].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
    // Fixups: save each sector's last two bytes, put the sequence number there.
    for i in 1..3usize {
        let end = i * 512;
        let saved = [rec[end - 2], rec[end - 1]];
        rec[0x30 + i * 2..0x32 + i * 2].copy_from_slice(&saved);
        rec[end - 2..end].copy_from_slice(&usn);
    }
    w.push((mft_lcn * cluster + 6 * 1024, rec));
    let split = (first_len * cluster) as usize;
    let (one, two) = bitmap.split_at(split.min(bitmap.len()));
    w.push((20 * cluster, one.to_vec()));
    if !two.is_empty() {
        w.push((40 * cluster, two.to_vec()));
    }
    w
}

/// Applies writes at `base` to an in-memory disk.
pub fn put(disk: &mut [u8], base: u64, writes: &[(u64, Vec<u8>)]) {
    for (o, b) in writes {
        let at = (base + o) as usize;
        disk[at..at + b.len()].copy_from_slice(b);
    }
}

/// A `Boot####` efivarfs file: attributes, then the load option.
pub fn load_option(label: &str, partition: Option<&str>, file: &str) -> Vec<u8> {
    let mut path = Vec::new();
    if let Some(g) = partition {
        let mut hd = vec![4u8, 1, 42, 0];
        hd.extend(1u32.to_le_bytes());
        hd.extend(2048u64.to_le_bytes());
        hd.extend(204800u64.to_le_bytes());
        hd.extend(guid_bytes(g));
        hd.push(2);
        hd.push(2);
        path.extend(hd);
    }
    let f: Vec<u8> = file.encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes()).collect();
    path.extend([4u8, 4]);
    path.extend(((4 + f.len()) as u16).to_le_bytes());
    path.extend(f);
    path.extend([0x7f, 0xff, 4, 0]);
    let mut v = 7u32.to_le_bytes().to_vec();
    v.extend(1u32.to_le_bytes());
    v.extend((path.len() as u16).to_le_bytes());
    v.extend(label.encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes()));
    v.extend(path);
    v
}
