// SPDX-License-Identifier: GPL-2.0-or-later
//! File systems told from their superblock magic, without mounting, and the
//! bytes they use where the superblock (or a structure it points to, read in
//! bounded amounts) says so cheaply: ext2/3/4, btrfs, XFS, FAT, NTFS. Where a
//! file system can't tell, `used` stays unknown and the plan keeps its upper
//! bound. All reads go through a [`Window`] onto the one partition.
//!
//! Counts a mounted file system keeps in memory (ext4's free blocks, XFS's
//! lazy counters) reach the superblock late, so `used` is an estimate: the
//! plan uses it only to tighten an upper bound, never to refuse.

use crate::table::{ReadAt, Window, le16, le32, le64, read_vec};

/// The most a probe reads beyond the superblocks to count used space (a FAT,
/// the NTFS cluster bitmap). Past it, `used` stays unknown.
pub const MAX_COUNT_READ: u64 = 64 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probed {
    /// The name blkid gives it: ext2, ext3, ext4, btrfs, xfs, vfat, exfat,
    /// ntfs, swap, crypto_LUKS.
    pub kind: String,
    pub used: Option<u64>,
}

fn probed(kind: &str, used: Option<u64>) -> Option<Probed> {
    Some(Probed { kind: kind.into(), used })
}

/// The file system on a partition of `size` bytes at `start`, if its magic
/// is one of the known ones. A read error ends the probe with nothing.
pub fn probe(disk: &mut dyn ReadAt, start: u64, size: u64) -> Option<Probed> {
    let mut w = Window { disk, start, size };
    probe_window(&mut w).ok().flatten()
}

fn probe_window(w: &mut Window) -> Result<Option<Probed>, String> {
    let size = w.size;
    if size < 4096 {
        return Ok(None);
    }
    let head = read_vec(w, 0, 4096)?;
    if &head[0..6] == b"LUKS\xba\xbe" {
        return Ok(probed("crypto_LUKS", None));
    }
    if &head[0..4] == b"XFSB" {
        return Ok(probed("xfs", xfs_used(&head)));
    }
    if &head[3..11] == b"NTFS    " {
        return Ok(probed("ntfs", ntfs_used(w, &head).ok().flatten()));
    }
    if &head[3..11] == b"EXFAT   " {
        return Ok(probed("exfat", None));
    }
    if le16(&head, 1024 + 0x38) == 0xef53 {
        return Ok(Some(ext(&head[1024..2048])));
    }
    if &head[4086..4096] == b"SWAPSPACE2" || &head[4086..4096] == b"SWAP-SPACE" {
        return Ok(probed("swap", None));
    }
    if size >= 0x10000 + 4096 {
        let sb = read_vec(w, 0x10000, 4096)?;
        if &sb[0x40..0x48] == b"_BHRfS_M" {
            let (total, used) = (le64(&sb, 0x70), le64(&sb, 0x78));
            return Ok(probed("btrfs", (used <= total).then_some(used)));
        }
    }
    if let Some(f) = fat(w, &head)? {
        return Ok(Some(f));
    }
    Ok(None)
}

fn ext(sb: &[u8]) -> Probed {
    let (compat, incompat) = (le32(sb, 0x5c), le32(sb, 0x60));
    let kind = if incompat & (0x40 | 0x80 | 0x200) != 0 {
        "ext4"
    } else if compat & 0x4 != 0 {
        "ext3"
    } else {
        "ext2"
    };
    let log = le32(sb, 0x18);
    let used = (log <= 6).then(|| {
        let bs = 1024u64 << log;
        let (mut blocks, mut free) = (le32(sb, 0x04) as u64, le32(sb, 0x0c) as u64);
        if incompat & 0x80 != 0 {
            blocks |= (le32(sb, 0x150) as u64) << 32;
            free |= (le32(sb, 0x158) as u64) << 32;
        }
        (free <= blocks).then(|| (blocks - free) * bs)
    });
    Probed {
        kind: kind.into(),
        used: used.flatten(),
    }
}

fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes(b[o..o + 4].try_into().unwrap())
}
fn be64(b: &[u8], o: usize) -> u64 {
    u64::from_be_bytes(b[o..o + 8].try_into().unwrap())
}

fn xfs_used(sb: &[u8]) -> Option<u64> {
    let (bs, blocks, free) = (be32(sb, 4) as u64, be64(sb, 8), be64(sb, 0x90));
    (bs.is_power_of_two() && (512..=65536).contains(&bs) && free <= blocks).then(|| (blocks - free) * bs)
}

/// FAT12/16/32 (blkid's `vfat`): the used clusters from FAT32's FSInfo when
/// it holds a count, else from the FAT itself, plus the reserved area, the
/// FATs and the root directory.
fn fat(w: &mut Window, b: &[u8]) -> Result<Option<Probed>, String> {
    if b[510] != 0x55 || b[511] != 0xaa || !matches!(b[0], 0xeb | 0xe9) {
        return Ok(None);
    }
    let bps = le16(b, 11) as u64;
    let spc = b[13] as u64;
    let reserved = le16(b, 14) as u64;
    let nfats = b[16] as u64;
    let root_entries = le16(b, 17) as u64;
    let total = match le16(b, 19) as u64 {
        0 => le32(b, 32) as u64,
        n => n,
    };
    let fatsz = match le16(b, 22) as u64 {
        0 => le32(b, 36) as u64,
        n => n,
    };
    if !bps.is_power_of_two() || !(512..=4096).contains(&bps) || !spc.is_power_of_two() || nfats == 0 || reserved == 0 || fatsz == 0 {
        return Ok(None);
    }
    let root_sectors = (root_entries * 32).div_ceil(bps);
    let meta = reserved + nfats * fatsz + root_sectors;
    if total <= meta || total * bps > w.size {
        return Ok(None);
    }
    let clusters = (total - meta) / spc;
    let bits = if clusters < 4085 {
        12
    } else if clusters < 65525 {
        16
    } else {
        32
    };
    let fat32 = bits == 32;
    let mut used_clusters = None;
    if fat32 {
        let fsinfo = le16(b, 48) as u64;
        if fsinfo != 0 && fsinfo < reserved {
            let fi = read_vec(w, fsinfo * bps, 512)?;
            let free = le32(&fi, 488) as u64;
            if le32(&fi, 0) == 0x4161_5252 && le32(&fi, 484) == 0x6141_7272 && free != 0xffff_ffff && free <= clusters {
                used_clusters = Some(clusters - free);
            }
        }
    }
    if used_clusters.is_none() {
        let fat_bytes = ((clusters + 2) * bits).div_ceil(8);
        if fat_bytes <= MAX_COUNT_READ {
            let t = read_vec(w, reserved * bps, fat_bytes as usize)?;
            let mut n = 0u64;
            for c in 2..clusters + 2 {
                let v: u32 = match bits {
                    12 => {
                        let x = le16(&t, (c * 3 / 2) as usize);
                        (if c % 2 == 0 { x & 0xfff } else { x >> 4 }) as u32
                    }
                    16 => le16(&t, (c * 2) as usize) as u32,
                    _ => le32(&t, (c * 4) as usize) & 0x0fff_ffff,
                };
                if v != 0 {
                    n += 1;
                }
            }
            used_clusters = Some(n);
        }
    }
    Ok(probed("vfat", used_clusters.map(|n| (meta + n * spc) * bps)))
}

/// NTFS keeps no free count in its boot sector: the used clusters are
/// counted in `$Bitmap` (MFT record 6), found through the MFT.
fn ntfs_used(w: &mut Window, b: &[u8]) -> Result<Option<u64>, String> {
    let bps = le16(b, 11) as u64;
    let spc = match b[13] {
        x if x <= 0x80 => x as u64,
        x => 1u64 << (256 - x as u32).min(31),
    };
    if !bps.is_power_of_two() || !(256..=4096).contains(&bps) || spc == 0 {
        return Ok(None);
    }
    let cluster = bps * spc;
    let total_clusters = le64(b, 0x28) / spc;
    let mft_lcn = le64(b, 0x30);
    let rec = match b[0x40] as i8 {
        x if x > 0 => x as u64 * cluster,
        x if x < 0 && x > -31 => 1u64 << (-(x as i32)),
        _ => return Ok(None),
    };
    if !(512..=65536).contains(&rec) || total_clusters == 0 {
        return Ok(None);
    }
    let at = mft_lcn.checked_mul(cluster).and_then(|x| x.checked_add(6 * rec)).ok_or("MFT past the end")?;
    let mut r = read_vec(w, at, rec as usize)?;
    if &r[0..4] != b"FILE" {
        return Ok(None);
    }
    // Update sequence fixups: the last two bytes of every sector.
    let (usa, count) = (le16(&r, 4) as usize, le16(&r, 6) as usize);
    if count < 2 || usa + count * 2 > r.len() || (count - 1) * bps as usize > r.len() {
        return Ok(None);
    }
    let check = [r[usa], r[usa + 1]];
    for i in 1..count {
        let end = i * bps as usize;
        if r[end - 2..end] != check {
            return Ok(None);
        }
        r[end - 2] = r[usa + i * 2];
        r[end - 1] = r[usa + i * 2 + 1];
    }
    let mut a = le16(&r, 0x14) as usize;
    while a + 16 <= r.len() {
        let (ty, len) = (le32(&r, a), le32(&r, a + 4) as usize);
        if ty == 0xffff_ffff || len < 16 || a + len > r.len() {
            break;
        }
        let attr = &r[a..a + len];
        // The unnamed, non-resident $DATA.
        if ty == 0x80 && attr[8] == 1 && attr[9] == 0 && len >= 0x40 {
            let runs = &attr[le16(attr, 0x20) as usize..];
            let real = le64(attr, 0x30).min(total_clusters.div_ceil(8));
            if real > MAX_COUNT_READ {
                return Ok(None);
            }
            let bitmap = read_runs(w, runs, cluster, real)?;
            let Some(bitmap) = bitmap else { return Ok(None) };
            let mut used = 0u64;
            for (i, byte) in bitmap.iter().enumerate() {
                let bits = if (i as u64 + 1) * 8 <= total_clusters {
                    *byte
                } else {
                    let keep = total_clusters.saturating_sub(i as u64 * 8) as u32;
                    byte & ((1u16 << keep) - 1) as u8
                };
                used += bits.count_ones() as u64;
            }
            return Ok(Some(used * cluster));
        }
        a += len;
    }
    Ok(None)
}

/// The first `want` bytes of a non-resident attribute, from its run list.
fn read_runs(w: &mut Window, mut runs: &[u8], cluster: u64, want: u64) -> Result<Option<Vec<u8>>, String> {
    let mut out = Vec::with_capacity(want as usize);
    let mut lcn: i64 = 0;
    while (out.len() as u64) < want {
        let Some(&h) = runs.first() else { return Ok(None) };
        if h == 0 {
            return Ok(None);
        }
        let (ls, os) = ((h & 0xf) as usize, (h >> 4) as usize);
        if ls == 0 || ls > 8 || os > 8 || runs.len() < 1 + ls + os {
            return Ok(None);
        }
        let mut len = 0u64;
        for i in 0..ls {
            len |= (runs[1 + i] as u64) << (8 * i);
        }
        if os == 0 {
            return Ok(None); // sparse: the bitmap is never sparse
        }
        let mut off = 0i64;
        for i in 0..os {
            off |= (runs[1 + ls + i] as i64) << (8 * i);
        }
        if runs[ls + os] & 0x80 != 0 && os < 8 {
            off -= 1i64 << (8 * os); // sign-extend
        }
        lcn += off;
        if lcn < 0 {
            return Ok(None);
        }
        let n = (len * cluster).min(want - out.len() as u64);
        out.extend(read_vec(w, lcn as u64 * cluster, n as usize)?);
        runs = &runs[1 + ls + os..];
    }
    Ok(Some(out))
}
