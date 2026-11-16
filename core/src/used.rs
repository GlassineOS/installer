// SPDX-License-Identifier: GPL-2.0-or-later
//! Which bytes of a partition its file system uses, from the file system's
//! own allocation metadata, read-only: ext2/3/4's block bitmaps, NTFS's
//! `$Bitmap`, the FAT, and btrfs's chunk tree (every chunk's stripes on this
//! device, a safe superset of the extents in them). The backup stores these
//! bytes and leaves the rest of the partition out.
//!
//! Never under-copy: a file system this doesn't know, one with a feature it
//! doesn't model, one whose metadata fails a check (a checksum, a free count,
//! the two copies of a table that must agree), or one that wasn't shut down
//! cleanly (a journal or log not replayed: its bitmaps lag) is used whole,
//! with the reason. What lies past the file system's own end, up to the
//! partition's, is always used.

use crate::table::{ReadAt, Window, le16, le32, le64, read_vec};

/// A partition's allocation as found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alloc {
    /// The file system, as blkid names it, when one was recognised.
    pub kind: Option<String>,
    /// The used byte ranges `[start, end)` relative to the partition, sorted
    /// and merged; or why the whole partition counts as used.
    pub used: Result<Vec<(u64, u64)>, String>,
}

/// The most of a bitmap or FAT read at once.
const STEP: usize = 4 << 20;
/// The largest allocation table read (a 2 TiB NTFS with 4 KiB clusters has
/// a 64 MiB `$Bitmap`; a FAT32 of 2 TiB a 256 MiB FAT).
const MAX_TABLE: u64 = 512 << 20;

/// The allocation of the partition `[start, start + size)` of `disk`.
pub fn partition(disk: &mut dyn ReadAt, start: u64, size: u64) -> Alloc {
    let mut w = Window { disk, start, size };
    let (kind, r) = match identify(&mut w) {
        Ok(Some(k)) => {
            let r = match k {
                "ext4" | "ext3" | "ext2" => ext(&mut w),
                "btrfs" => btrfs(&mut w),
                "ntfs" => ntfs(&mut w),
                "vfat" => fat(&mut w),
                _ => Err(format!("{k}: its free space isn't read, so it is kept whole")),
            };
            (Some(k.to_string()), r)
        }
        Ok(None) => (None, Err("no file system this installer reads: kept whole".to_string())),
        Err(e) => (None, Err(format!("couldn't be read ({e}): kept whole"))),
    };
    let used = r.and_then(|mut v| {
        normalize(&mut v);
        match v.last() {
            Some(&(_, e)) if e > size => Err("the file system reaches past its partition: kept whole".to_string()),
            _ => Ok(v),
        }
    });
    Alloc { kind, used }
}

fn identify(w: &mut Window) -> Result<Option<&'static str>, String> {
    if w.size < 4096 {
        return Ok(None);
    }
    let head = read_vec(w, 0, 4096)?;
    if &head[3..11] == b"NTFS    " {
        return Ok(Some("ntfs"));
    }
    if &head[3..11] == b"EXFAT   " {
        return Ok(Some("exfat"));
    }
    if le16(&head, 1024 + 0x38) == 0xef53 {
        return Ok(Some("ext4"));
    }
    if &head[0..4] == b"XFSB" {
        return Ok(Some("xfs"));
    }
    if &head[0..6] == b"LUKS\xba\xbe" {
        return Ok(Some("crypto_LUKS"));
    }
    if w.size >= 0x10000 + 4096 {
        let sb = read_vec(w, 0x10000, 4096)?;
        if &sb[0x40..0x48] == b"_BHRfS_M" {
            return Ok(Some("btrfs"));
        }
    }
    if fat_bpb(&head, w.size).is_some() {
        return Ok(Some("vfat"));
    }
    Ok(None)
}

/// Sorts and merges ranges, dropping empty ones.
pub fn normalize(v: &mut Vec<(u64, u64)>) {
    v.retain(|(a, b)| b > a);
    v.sort_unstable();
    let mut out: Vec<(u64, u64)> = Vec::with_capacity(v.len());
    for &(a, b) in v.iter() {
        match out.last_mut() {
            Some(l) if a <= l.1 => l.1 = l.1.max(b),
            _ => out.push((a, b)),
        }
    }
    *v = out;
}

/// Ranges of set bits of `bits` (bit i of byte i/8, least significant
/// first), for units `first..first + n`, each `unit` bytes, offset by `base`.
fn bit_ranges(bits: &[u8], n: u64, first: u64, unit: u64, base: u64, out: &mut Vec<(u64, u64)>) {
    let mut run: Option<u64> = None;
    let mut i = 0u64;
    while i < n {
        let byte = bits[(i / 8) as usize];
        // whole bytes at once where they are all set or all clear
        if i.is_multiple_of(8) && i + 8 <= n && (byte == 0 || byte == 0xff) {
            match (byte, run) {
                (0xff, None) => run = Some(i),
                (0, Some(s)) => {
                    push(out, base + (first + s) * unit, base + (first + i) * unit);
                    run = None;
                }
                _ => {}
            }
            i += 8;
            continue;
        }
        let set = byte >> (i % 8) & 1 == 1;
        match (set, run) {
            (true, None) => run = Some(i),
            (false, Some(s)) => {
                push(out, base + (first + s) * unit, base + (first + i) * unit);
                run = None;
            }
            _ => {}
        }
        i += 1;
    }
    if let Some(s) = run {
        push(out, base + (first + s) * unit, base + (first + n) * unit);
    }
}

fn push(out: &mut Vec<(u64, u64)>, a: u64, b: u64) {
    match out.last_mut() {
        Some(l) if l.1 == a => l.1 = b,
        _ => out.push((a, b)),
    }
}

fn count_clear(bits: &[u8], n: u64) -> u64 {
    let full = (n / 8) as usize;
    let mut c: u64 = bits[..full].iter().map(|b| b.count_zeros() as u64).sum();
    for i in (full as u64 * 8)..n {
        if bits[(i / 8) as usize] >> (i % 8) & 1 == 0 {
            c += 1;
        }
    }
    c
}

// ---- checksums ---------------------------------------------------------------------

/// CRC-32C's register after `data`, from `crc`, without the inversions
/// before and after (ext4's and btrfs's `crc32c(seed, ...)`).
pub fn crc32c_raw(mut crc: u32, data: &[u8]) -> u32 {
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0x82f6_3b78 } else { crc >> 1 };
        }
    }
    crc
}

/// The CRC-16 (polynomial 0x8005, reflected) ext4's older group descriptor
/// checksum uses.
fn crc16(mut crc: u16, data: &[u8]) -> u16 {
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xa001 } else { crc >> 1 };
        }
    }
    crc
}

// ---- ext2/3/4 ----------------------------------------------------------------------

const EXT_INCOMPAT_RECOVER: u32 = 0x4;
const EXT_INCOMPAT_JOURNAL_DEV: u32 = 0x8;
const EXT_INCOMPAT_META_BG: u32 = 0x10;
const EXT_INCOMPAT_64BIT: u32 = 0x80;
const EXT_INCOMPAT_CSUM_SEED: u32 = 0x2000;
/// filetype, extents, 64bit, mmp, flex_bg, ea_inode, dirdata, csum_seed,
/// largedir, inline_data, encrypt, casefold
const EXT_INCOMPAT_KNOWN: u32 = 0x2 | 0x40 | 0x80 | 0x100 | 0x200 | 0x400 | 0x1000 | 0x2000 | 0x4000 | 0x8000 | 0x10000 | 0x20000;
const EXT_RO_SPARSE_SUPER: u32 = 0x1;
const EXT_RO_GDT_CSUM: u32 = 0x10;
const EXT_RO_METADATA_CSUM: u32 = 0x400;
/// sparse_super, large_file, btree_dir, huge_file, gdt_csum, dir_nlink,
/// extra_isize, quota, metadata_csum, readonly, project, verity,
/// orphan_present (bigalloc, 0x200, is not: its bitmaps count clusters)
const EXT_RO_KNOWN: u32 = 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | 0x20 | 0x40 | 0x100 | 0x400 | 0x1000 | 0x2000 | 0x8000 | 0x10000;
const EXT_COMPAT_RESIZE_INODE: u32 = 0x10;
const EXT_COMPAT_SPARSE_SUPER2: u32 = 0x200;
const EXT_BG_BLOCK_UNINIT: u16 = 0x2;

struct Group {
    block_bitmap: u64,
    inode_bitmap: u64,
    inode_table: u64,
    free: u64,
    flags: u16,
    bitmap_csum: u32,
}

fn is_power_of(mut g: u64, b: u64) -> bool {
    while g > 1 && g.is_multiple_of(b) {
        g /= b;
    }
    g == 1
}

fn ext(w: &mut Window) -> Result<Vec<(u64, u64)>, String> {
    let full = |why: &str| Err(format!("ext4: {why}: kept whole"));
    let sb = read_vec(w, 1024, 1024)?;
    let (compat, incompat, ro) = (le32(&sb, 0x5c), le32(&sb, 0x60), le32(&sb, 0x64));
    if incompat & EXT_INCOMPAT_RECOVER != 0 {
        return full("its journal holds changes not yet written (not shut down cleanly)");
    }
    if incompat & EXT_INCOMPAT_JOURNAL_DEV != 0 {
        return full("an external journal");
    }
    if incompat & EXT_INCOMPAT_META_BG != 0 || compat & EXT_COMPAT_SPARSE_SUPER2 != 0 {
        return full("a layout of its group descriptors this installer doesn't read (meta_bg, sparse_super2)");
    }
    if incompat & !EXT_INCOMPAT_KNOWN != 0 || ro & !EXT_RO_KNOWN != 0 {
        return full("features this installer doesn't know (bigalloc or newer)");
    }
    let state = le16(&sb, 0x3a);
    if state & 1 == 0 || state & 2 != 0 {
        return full("not cleanly unmounted, or errors recorded");
    }
    let csum = ro & EXT_RO_METADATA_CSUM != 0;
    if csum && (sb[0x175] != 1 || crc32c_raw(!0, &sb[..0x3fc]) != le32(&sb, 0x3fc)) {
        return full("its superblock's checksum doesn't match");
    }
    let log = le32(&sb, 0x18);
    if log > 6 {
        return full("an impossible block size");
    }
    let bs = 1024u64 << log;
    let wide = incompat & EXT_INCOMPAT_64BIT != 0;
    let mut blocks = le32(&sb, 0x04) as u64;
    if wide {
        blocks |= (le32(&sb, 0x150) as u64) << 32;
    }
    let first = le32(&sb, 0x14) as u64;
    let bpg = le32(&sb, 0x20) as u64;
    let ipg = le32(&sb, 0x28) as u64;
    let inode_size = if le32(&sb, 0x4c) >= 1 { le16(&sb, 0x58) as u64 } else { 128 };
    if bpg == 0 || bpg != le32(&sb, 0x24) as u64 || bpg > 8 * bs || !bpg.is_multiple_of(8) || first > 1 || blocks <= first {
        return full("an impossible group size");
    }
    if blocks.checked_mul(bs).is_none_or(|b| b > w.size) {
        return full("larger than its partition");
    }
    let groups = (blocks - first).div_ceil(bpg);
    let desc = if wide { le16(&sb, 0xfe) as u64 } else { 32 };
    if !(32..=1024).contains(&desc) || !desc.is_power_of_two() {
        return full("an impossible descriptor size");
    }
    let seed = if incompat & EXT_INCOMPAT_CSUM_SEED != 0 {
        le32(&sb, 0x270)
    } else {
        crc32c_raw(!0, &sb[0x68..0x78])
    };
    let gdt_blocks = (groups * desc).div_ceil(bs);
    let reserved_gdt = if compat & EXT_COMPAT_RESIZE_INODE != 0 { le16(&sb, 0xce) as u64 } else { 0 };
    if gdt_blocks * bs > MAX_TABLE {
        return full("too many groups to read");
    }
    let gdt = read_vec(w, (first + 1) * bs, (gdt_blocks * bs) as usize)?;
    let gdt_csum = ro & EXT_RO_GDT_CSUM != 0;
    let mut gs = Vec::with_capacity(groups as usize);
    for g in 0..groups {
        let d = &gdt[(g * desc) as usize..((g + 1) * desc) as usize];
        let hi = |o: usize| if wide && desc >= 64 { le32(d, o) as u64 } else { 0 };
        if csum {
            let mut c = crc32c_raw(seed, &(g as u32).to_le_bytes());
            c = crc32c_raw(c, &d[..0x1e]);
            c = crc32c_raw(c, &[0, 0]);
            if desc > 0x20 {
                c = crc32c_raw(c, &d[0x20..]);
            }
            if (c & 0xffff) as u16 != le16(d, 0x1e) {
                return full(&format!("group {g}'s descriptor checksum doesn't match"));
            }
        } else if gdt_csum {
            let mut c = crc16(0xffff, &sb[0x68..0x78]);
            c = crc16(c, &(g as u32).to_le_bytes());
            c = crc16(c, &d[..0x1e]);
            if wide && desc > 0x20 {
                c = crc16(c, &d[0x20..]);
            }
            if c != le16(d, 0x1e) {
                return full(&format!("group {g}'s descriptor checksum doesn't match"));
            }
        }
        let mut bitmap_csum = le16(d, 0x18) as u32;
        if desc >= 0x3a {
            bitmap_csum |= (le16(d, 0x38) as u32) << 16;
        }
        gs.push(Group {
            block_bitmap: le32(d, 0) as u64 | hi(0x20) << 32,
            inode_bitmap: le32(d, 4) as u64 | hi(0x24) << 32,
            inode_table: le32(d, 8) as u64 | hi(0x28) << 32,
            free: le16(d, 0xc) as u64 | (if wide && desc >= 64 { le16(d, 0x2c) as u64 } else { 0 }) << 16,
            flags: le16(d, 0x12),
            bitmap_csum,
        });
    }
    let it_blocks = (ipg * inode_size).div_ceil(bs);
    let mut used: Vec<(u64, u64)> = vec![(0, first * bs)];
    let sparse = ro & EXT_RO_SPARSE_SUPER != 0;
    for (g, gr) in gs.iter().enumerate() {
        let g = g as u64;
        if g == 0 || !sparse || is_power_of(g, 3) || is_power_of(g, 5) || is_power_of(g, 7) {
            let s = first + g * bpg;
            used.push((s * bs, (s + 1 + gdt_blocks + reserved_gdt).min(blocks) * bs));
        }
        for (b, n) in [(gr.block_bitmap, 1), (gr.inode_bitmap, 1), (gr.inode_table, it_blocks)] {
            if b < first || b + n > blocks {
                return full(&format!("group {g}'s metadata lies outside the file system"));
            }
            used.push((b * bs, (b + n) * bs));
        }
    }
    // The block bitmaps, read in runs of neighbouring blocks (flex_bg keeps
    // them together).
    let uninit_ok = csum || gdt_csum;
    let mut order: Vec<u64> = (0..groups)
        .filter(|g| !(uninit_ok && gs[*g as usize].flags & EXT_BG_BLOCK_UNINIT != 0))
        .collect();
    order.sort_by_key(|g| gs[*g as usize].block_bitmap);
    let per = (STEP as u64 / bs).max(1);
    let mut i = 0;
    while i < order.len() {
        let b0 = gs[order[i] as usize].block_bitmap;
        let mut j = i + 1;
        while j < order.len() && gs[order[j] as usize].block_bitmap == b0 + (j - i) as u64 && ((j - i) as u64) < per {
            j += 1;
        }
        let buf = read_vec(w, b0 * bs, ((j - i) as u64 * bs) as usize)?;
        for (k, &g) in order[i..j].iter().enumerate() {
            let gr = &gs[g as usize];
            let bits = &buf[k * bs as usize..(k + 1) * bs as usize];
            if csum {
                let c = crc32c_raw(seed, &bits[..(bpg / 8) as usize]);
                let mask = if desc >= 0x3a { !0 } else { 0xffff };
                if c & mask != gr.bitmap_csum {
                    return full(&format!("group {g}'s block bitmap checksum doesn't match"));
                }
            }
            let n = bpg.min(blocks - first - g * bpg);
            if count_clear(bits, n) != gr.free {
                return full(&format!("group {g}'s block bitmap and its free count disagree"));
            }
            bit_ranges(bits, n, first + g * bpg, bs, 0, &mut used);
        }
        i = j;
    }
    used.push((blocks * bs, w.size));
    Ok(used)
}

// ---- btrfs -------------------------------------------------------------------------

const BTRFS_SB: u64 = 0x10000;
/// mixed_backref, default_subvol, mixed_groups, compress_lzo, compress_zstd,
/// big_metadata, extended_iref, raid56, skinny_metadata, no_holes,
/// metadata_uuid, raid1c34, simple_quota (zoned, extent tree v2 and the raid
/// stripe tree are not)
const BTRFS_INCOMPAT_KNOWN: u64 = 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | 0x20 | 0x40 | 0x80 | 0x100 | 0x200 | 0x400 | 0x800 | 0x10000;
const BTRFS_INCOMPAT_METADATA_UUID: u64 = 0x400;
/// raid0, raid1, raid10, raid5, raid6, raid1c3, raid1c4: stripes elsewhere
const BTRFS_STRIPED: u64 = 0x8 | 0x10 | 0x40 | 0x80 | 0x100 | 0x200 | 0x400;
const BTRFS_CHUNK_ITEM: u8 = 228;

struct Chunk {
    logical: u64,
    length: u64,
    kind: u64,
    stripes: Vec<(u64, u64)>,
}

fn chunk_item(b: &[u8], logical: u64) -> Option<Chunk> {
    if b.len() < 48 {
        return None;
    }
    let n = le16(b, 44) as usize;
    if n == 0 || b.len() < 48 + n * 32 {
        return None;
    }
    Some(Chunk {
        logical,
        length: le64(b, 0),
        kind: le64(b, 24),
        stripes: (0..n).map(|i| (le64(b, 48 + i * 32), le64(b, 56 + i * 32))).collect(),
    })
}

fn btrfs(w: &mut Window) -> Result<Vec<(u64, u64)>, String> {
    let full = |why: &str| Err(format!("btrfs: {why}: kept whole"));
    let sb = read_vec(w, BTRFS_SB, 4096)?;
    if le16(&sb, 0xc4) != 0 {
        return full("a checksum other than crc32c");
    }
    if !crc32c_raw(!0, &sb[0x20..]) != le32(&sb, 0) || le64(&sb, 0x30) != BTRFS_SB {
        return full("its superblock's checksum doesn't match");
    }
    let incompat = le64(&sb, 0xbc);
    if incompat & !BTRFS_INCOMPAT_KNOWN != 0 {
        return full("features this installer doesn't know (zoned, extent tree v2 or newer)");
    }
    if le64(&sb, 0x88) != 1 {
        return full("it spans more than one device");
    }
    let nodesize = le32(&sb, 0x94) as u64;
    if !nodesize.is_power_of_two() || !(4096..=65536).contains(&nodesize) {
        return full("an impossible node size");
    }
    let dev = &sb[0xc9..0xc9 + 98];
    let (devid, dev_total, dev_used) = (le64(dev, 0), le64(dev, 8), le64(dev, 16));
    if dev_total > w.size || dev_used > dev_total {
        return full("larger than its partition");
    }
    let fsid: [u8; 16] = if incompat & BTRFS_INCOMPAT_METADATA_UUID != 0 {
        sb[0x23b..0x24b].try_into().unwrap()
    } else {
        sb[0x20..0x30].try_into().unwrap()
    };
    // The bootstrap map: the system chunks the superblock carries.
    let n = le32(&sb, 0xa0) as usize;
    if n > 2048 {
        return full("an impossible system chunk array");
    }
    let arr = &sb[0x32b..0x32b + n];
    let mut sys = vec![];
    let mut at = 0;
    while at + 17 <= n {
        if arr[at + 8] != BTRFS_CHUNK_ITEM {
            return full("an unexpected item in the system chunk array");
        }
        let logical = le64(arr, at + 9);
        at += 17;
        let Some(c) = chunk_item(&arr[at..], logical) else {
            return full("a damaged system chunk array");
        };
        at += 48 + c.stripes.len() * 32;
        sys.push(c);
    }
    let map = |sys: &[Chunk], l: u64| -> Option<u64> {
        let c = sys.iter().find(|c| l >= c.logical && l + nodesize <= c.logical + c.length)?;
        if c.kind & BTRFS_STRIPED != 0 {
            return None;
        }
        let &(d, o) = c.stripes.first()?;
        (d == devid).then_some(o + (l - c.logical))
    };
    // The chunk tree, every node checked.
    let mut chunks = vec![];
    let mut stack = vec![(le64(&sb, 0x58), sb[0xc7])];
    let mut nodes = 0;
    while let Some((l, level)) = stack.pop() {
        nodes += 1;
        if nodes > 1 << 16 {
            return full("a chunk tree too large to be real");
        }
        let Some(p) = map(&sys, l) else {
            return full("a chunk tree node outside the system chunks");
        };
        if p + nodesize > dev_total {
            return full("a chunk tree node past the device's end");
        }
        let b = read_vec(w, p, nodesize as usize)?;
        if !crc32c_raw(!0, &b[0x20..]) != le32(&b, 0) || le64(&b, 0x30) != l || b[0x20..0x30] != fsid || b[0x64] != level {
            return full("a chunk tree node fails its checks");
        }
        let items = le32(&b, 0x60) as usize;
        if level == 0 {
            if 0x65 + items * 25 > b.len() {
                return full("a damaged chunk tree leaf");
            }
            for i in 0..items {
                let it = &b[0x65 + i * 25..0x65 + (i + 1) * 25];
                if it[8] != BTRFS_CHUNK_ITEM {
                    continue;
                }
                let (off, sz) = (0x65 + le32(it, 17) as usize, le32(it, 21) as usize);
                let Some(c) = b.get(off..off + sz).and_then(|d| chunk_item(d, le64(it, 9))) else {
                    return full("a damaged chunk item");
                };
                chunks.push(c);
            }
        } else {
            if 0x65 + items * 33 > b.len() {
                return full("a damaged chunk tree node");
            }
            for i in 0..items {
                stack.push((le64(&b, 0x65 + i * 33 + 17), level - 1));
            }
        }
    }
    let mut used = vec![(0, (1u64 << 20).min(w.size))];
    let mut on_dev = 0u64;
    for c in &chunks {
        if c.kind & BTRFS_STRIPED != 0 {
            return full("a striped or mirrored profile");
        }
        for &(d, o) in &c.stripes {
            if d != devid {
                return full("a chunk on another device");
            }
            if o.checked_add(c.length).is_none_or(|e| e > dev_total) {
                return full("a chunk past the device's end");
            }
            used.push((o, o + c.length));
            on_dev += c.length;
        }
    }
    if on_dev != dev_used {
        return full("its chunks and the device's used bytes disagree");
    }
    // the superblock's mirrors
    for m in [64u64 << 20, 256 << 30] {
        if m + 4096 <= dev_total {
            used.push((m, m + 4096));
        }
    }
    used.push((dev_total, w.size));
    Ok(used)
}

// ---- NTFS --------------------------------------------------------------------------

struct Ntfs {
    bps: u64,
    cluster: u64,
    clusters: u64,
    mft: u64,
    record: u64,
}

impl Ntfs {
    /// MFT record `n` (one of the first ones, in the MFT's first run) with
    /// its update sequence applied; `None` when it doesn't check out.
    fn record(&self, w: &mut Window, n: u64) -> Result<Option<Vec<u8>>, String> {
        let at = self.mft * self.cluster + n * self.record;
        let r = read_vec(w, at, self.record as usize)?;
        Ok(self.fixup(r, b"FILE"))
    }

    fn fixup(&self, mut r: Vec<u8>, magic: &[u8; 4]) -> Option<Vec<u8>> {
        if &r[0..4] != magic {
            return None;
        }
        let (usa, count) = (le16(&r, 4) as usize, le16(&r, 6) as usize);
        if count < 2 || usa + count * 2 > r.len() || (count - 1) * 512 > r.len() {
            return None;
        }
        let check = [r[usa], r[usa + 1]];
        for i in 1..count {
            let end = i * 512;
            if r[end - 2..end] != check {
                return None;
            }
            r[end - 2] = r[usa + i * 2];
            r[end - 1] = r[usa + i * 2 + 1];
        }
        Some(r)
    }
}

/// The attributes of an MFT record: (type, the attribute's bytes).
fn attrs(r: &[u8]) -> Vec<(u32, &[u8])> {
    let mut out = vec![];
    let mut a = le16(r, 0x14) as usize;
    while a + 16 <= r.len() {
        let (ty, len) = (le32(r, a), le32(r, a + 4) as usize);
        if ty == 0xffff_ffff || len < 16 || a + len > r.len() {
            break;
        }
        out.push((ty, &r[a..a + len]));
        a += len;
    }
    out
}

/// A non-resident attribute's runs as (cluster, count); `None` when sparse
/// or damaged.
fn runs(attr: &[u8]) -> Option<Vec<(u64, u64)>> {
    if attr.len() < 0x40 || attr[8] != 1 {
        return None;
    }
    let mut r = attr.get(le16(attr, 0x20) as usize..)?;
    let mut out = vec![];
    let mut lcn: i64 = 0;
    loop {
        let &h = r.first()?;
        if h == 0 {
            return Some(out);
        }
        let (ls, os) = ((h & 0xf) as usize, (h >> 4) as usize);
        if ls == 0 || ls > 8 || os == 0 || os > 8 || r.len() < 1 + ls + os {
            return None;
        }
        let mut len = 0u64;
        for i in 0..ls {
            len |= (r[1 + i] as u64) << (8 * i);
        }
        let mut off = 0i64;
        for i in 0..os {
            off |= (r[1 + ls + i] as i64) << (8 * i);
        }
        if r[ls + os] & 0x80 != 0 && os < 8 {
            off -= 1i64 << (8 * os);
        }
        lcn += off;
        if lcn < 0 {
            return None;
        }
        out.push((lcn as u64, len));
        r = &r[1 + ls + os..];
    }
}

/// The first `want` bytes of the unnamed `$DATA` of an MFT record.
fn data_of(v: &Ntfs, w: &mut Window, rec: &[u8], want: u64) -> Result<Option<Vec<u8>>, String> {
    let all = attrs(rec);
    if all.iter().any(|(t, _)| *t == 0x20) {
        return Ok(None); // an attribute list: the data may be elsewhere
    }
    let Some((_, a)) = all.iter().find(|(t, a)| *t == 0x80 && a[9] == 0) else {
        return Ok(None);
    };
    if a[8] != 1 || le64(a, 0x30) < want {
        return Ok(None);
    }
    let Some(rs) = runs(a) else { return Ok(None) };
    let mut out = Vec::with_capacity(want as usize);
    for (lcn, len) in rs {
        if out.len() as u64 >= want {
            break;
        }
        if lcn.checked_add(len).is_none_or(|e| e > v.clusters) {
            return Ok(None);
        }
        let n = (len * v.cluster).min(want - out.len() as u64);
        out.extend(read_vec(w, lcn * v.cluster, n as usize)?);
    }
    Ok((out.len() as u64 == want).then_some(out))
}

/// Whether a `$LogFile` restart page says the volume is clean: the newest
/// restart area has no client in use or the clean flag. A log never used
/// (all 0xff, as mkntfs leaves it) is clean.
fn logfile_clean(v: &Ntfs, w: &mut Window, rec: &[u8]) -> Result<bool, String> {
    let Some(pages) = data_of(v, w, rec, 8192)? else { return Ok(false) };
    if pages[..4096].iter().all(|b| *b == 0xff) {
        return Ok(true);
    }
    let mut newest: Option<(u64, bool)> = None;
    for p in [&pages[..4096], &pages[4096..]] {
        let Some(p) = v.fixup(p.to_vec(), b"RSTR") else { continue };
        let ra = le16(&p, 0x18) as usize;
        if ra + 16 > p.len() {
            continue;
        }
        let (lsn, in_use, flags) = (le64(&p, ra), le16(&p, ra + 0xc), le16(&p, ra + 0xe));
        let clean = in_use == 0xffff || flags & 0x2 != 0;
        if newest.is_none_or(|(l, _)| lsn >= l) {
            newest = Some((lsn, clean));
        }
    }
    Ok(newest.is_some_and(|(_, c)| c))
}

fn ntfs(w: &mut Window) -> Result<Vec<(u64, u64)>, String> {
    let full = |why: &str| Err(format!("ntfs: {why}: kept whole"));
    let b = read_vec(w, 0, 512)?;
    let bps = le16(&b, 11) as u64;
    let spc = match b[13] {
        x if x <= 0x80 => x as u64,
        x => 1u64 << (256 - x as u32).min(31),
    };
    if !bps.is_power_of_two() || !(256..=4096).contains(&bps) || spc == 0 {
        return full("an impossible boot sector");
    }
    let cluster = bps * spc;
    let sectors = le64(&b, 0x28);
    if sectors.checked_mul(bps).is_none_or(|s| s > w.size) {
        return full("larger than its partition");
    }
    let record = match b[0x40] as i8 {
        x if x > 0 => x as u64 * cluster,
        x if x < 0 && x > -31 => 1u64 << (-(x as i32)),
        _ => return full("an impossible MFT record size"),
    };
    let v = Ntfs {
        bps,
        cluster,
        clusters: sectors / spc,
        mft: le64(&b, 0x30),
        record,
    };
    let mirror = le64(&b, 0x38);
    if !(1024..=65536).contains(&record) || v.clusters == 0 || v.mft >= v.clusters || mirror >= v.clusters {
        return full("an impossible boot sector");
    }
    let _ = v.bps;
    // $MFT's first records and their mirror agree.
    let n = (4 * record).div_ceil(cluster) * cluster;
    if mirror * cluster + n > w.size || v.mft * cluster + n > w.size {
        return full("its MFT lies outside the volume");
    }
    if read_vec(w, v.mft * cluster, (4 * record) as usize)? != read_vec(w, mirror * cluster, (4 * record) as usize)? {
        return full("$MFT and $MFTMirr disagree");
    }
    let Some(vol) = v.record(w, 3)? else { return full("$Volume can't be read") };
    let Some((_, info)) = attrs(&vol).into_iter().find(|(t, a)| *t == 0x70 && a[8] == 0) else {
        return full("$Volume has no volume information");
    };
    let vo = le16(info, 0x14) as usize;
    if vo + 12 > info.len() {
        return full("$Volume's information is damaged");
    }
    if le16(info, vo + 10) & 0x1 != 0 {
        return full("marked dirty (Windows wasn't shut down fully, or a check is due)");
    }
    let Some(log) = v.record(w, 2)? else { return full("$LogFile can't be read") };
    if !logfile_clean(&v, w, &log)? {
        return full("its log isn't clean (Windows' Fast Startup or hibernation)");
    }
    let Some(bm) = v.record(w, 6)? else { return full("$Bitmap can't be read") };
    let need = v.clusters.div_ceil(8);
    if need > MAX_TABLE {
        return full("a cluster bitmap too large to read");
    }
    let Some(bits) = data_of(&v, w, &bm, need)? else {
        return full("$Bitmap's data can't be read whole");
    };
    let set = |c: u64| bits[(c / 8) as usize] >> (c % 8) & 1 == 1;
    if !set(0) || !set(v.mft) || !set(mirror) {
        return full("$Bitmap doesn't mark the boot sector and the MFT as used");
    }
    let mut used = vec![];
    bit_ranges(&bits, v.clusters, 0, cluster, 0, &mut used);
    // past the last cluster: the boot sector's backup, at the volume's end
    used.push((v.clusters * cluster, w.size));
    Ok(used)
}

// ---- FAT ---------------------------------------------------------------------------

struct Bpb {
    bps: u64,
    spc: u64,
    reserved: u64,
    fats: u64,
    fat_sectors: u64,
    /// the first data sector
    data: u64,
    clusters: u64,
    bits: u64,
    media: u8,
    ext_flags: u16,
}

fn fat_bpb(b: &[u8], size: u64) -> Option<Bpb> {
    if b[510] != 0x55 || b[511] != 0xaa || !matches!(b[0], 0xeb | 0xe9) {
        return None;
    }
    let bps = le16(b, 11) as u64;
    let spc = b[13] as u64;
    let reserved = le16(b, 14) as u64;
    let fats = b[16] as u64;
    let root_entries = le16(b, 17) as u64;
    let total = match le16(b, 19) as u64 {
        0 => le32(b, 32) as u64,
        n => n,
    };
    let fat_sectors = match le16(b, 22) as u64 {
        0 => le32(b, 36) as u64,
        n => n,
    };
    if !bps.is_power_of_two() || !(512..=4096).contains(&bps) || !spc.is_power_of_two() || fats == 0 || reserved == 0 || fat_sectors == 0 {
        return None;
    }
    let data = reserved + fats * fat_sectors + (root_entries * 32).div_ceil(bps);
    if total <= data || total * bps > size {
        return None;
    }
    let clusters = (total - data) / spc;
    let bits = if clusters < 4085 {
        12
    } else if clusters < 65525 {
        16
    } else {
        32
    };
    Some(Bpb {
        bps,
        spc,
        reserved,
        fats,
        fat_sectors,
        data,
        clusters,
        bits,
        media: b[21],
        ext_flags: if bits == 32 { le16(b, 40) } else { 0 },
    })
}

fn fat_entry(t: &[u8], bits: u64, c: u64) -> u32 {
    match bits {
        12 => {
            let x = le16(t, (c * 3 / 2) as usize);
            (if c.is_multiple_of(2) { x & 0xfff } else { x >> 4 }) as u32
        }
        16 => le16(t, (c * 2) as usize) as u32,
        _ => le32(t, (c * 4) as usize) & 0x0fff_ffff,
    }
}

fn fat(w: &mut Window) -> Result<Vec<(u64, u64)>, String> {
    let full = |why: &str| Err(format!("vfat: {why}: kept whole"));
    let head = read_vec(w, 0, 512)?;
    let Some(p) = fat_bpb(&head, w.size) else {
        return full("an impossible boot sector");
    };
    let entries_bytes = ((p.clusters + 2) * p.bits).div_ceil(8);
    if entries_bytes > p.fat_sectors * p.bps {
        return full("its FAT is too small for its clusters");
    }
    if entries_bytes > MAX_TABLE {
        return full("a FAT too large to read");
    }
    // FAT32 may switch mirroring off and use one FAT: then only that one counts.
    let mirrored = p.ext_flags & 0x80 == 0;
    let active = if mirrored { 0 } else { (p.ext_flags & 0xf) as u64 };
    if active >= p.fats {
        return full("its active FAT doesn't exist");
    }
    let fat_at = |i: u64| (p.reserved + i * p.fat_sectors) * p.bps;
    let cs = p.spc * p.bps;
    let mut used = vec![(0, p.data * p.bps)];
    // Read in steps that hold whole entries (FAT12's entries straddle bytes:
    // its FAT is read whole, it is never large).
    let step = if p.bits == 12 { entries_bytes } else { (STEP as u64).min(entries_bytes) };
    let per = step * 8 / p.bits;
    let mut first = 0u64;
    let mut run: Option<u64> = None;
    while first < p.clusters + 2 {
        let n = per.min(p.clusters + 2 - first);
        let off = first * p.bits / 8;
        let len = (n * p.bits).div_ceil(8);
        let t = read_vec(w, fat_at(active) + off, len as usize)?;
        if mirrored {
            for i in 1..p.fats {
                if read_vec(w, fat_at(i) + off, len as usize)? != t {
                    return full("its FAT copies disagree");
                }
            }
        }
        for k in 0..n {
            let c = first + k;
            let e = fat_entry(&t, p.bits, k);
            if c == 0 {
                if e & 0xff != p.media as u32 {
                    return full("its FAT doesn't start with the media byte");
                }
                continue;
            }
            if c == 1 {
                // the clean-shutdown bit (FAT16: bit 15, FAT32: bit 27)
                let clean = match p.bits {
                    16 => e & 0x8000 != 0,
                    32 => e & 0x0800_0000 != 0,
                    _ => true,
                };
                if !clean {
                    return full("marked as not cleanly unmounted");
                }
                continue;
            }
            match (e != 0, run) {
                (true, None) => run = Some(c),
                (false, Some(s)) => {
                    push(&mut used, p.data * p.bps + (s - 2) * cs, p.data * p.bps + (c - 2) * cs);
                    run = None;
                }
                _ => {}
            }
        }
        first += n;
    }
    if let Some(s) = run {
        push(&mut used, p.data * p.bps + (s - 2) * cs, p.data * p.bps + p.clusters * cs);
    }
    used.push((p.data * p.bps + p.clusters * cs, w.size));
    Ok(used)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32c_is_the_standard_one() {
        assert_eq!(!crc32c_raw(!0, b"123456789"), 0xe306_9283);
    }

    #[test]
    fn crc16_is_arc() {
        assert_eq!(crc16(0, b"123456789"), 0xbb3d);
    }

    #[test]
    fn bit_runs() {
        let mut v = vec![];
        bit_ranges(&[0b0000_0110, 0xff, 0x01], 20, 10, 4, 100, &mut v);
        assert_eq!(v, vec![(100 + 11 * 4, 100 + 13 * 4), (100 + 18 * 4, 100 + 27 * 4)]);
        assert_eq!(count_clear(&[0b0000_0110, 0xff, 0x01], 20), 20 - 2 - 8 - 1);
    }

    #[test]
    fn ranges_merge() {
        let mut v = vec![(5, 9), (0, 2), (2, 3), (8, 12), (20, 20)];
        normalize(&mut v);
        assert_eq!(v, vec![(0, 3), (5, 12)]);
    }
}
