// SPDX-License-Identifier: GPL-2.0-or-later
//! A FAT32 file system laid out from a list of files, deterministically: the
//! same files, sizes, volume and serial give the same bytes, so a cut write
//! is redone whole with the same bytes. Pure: the layout says which bytes go
//! where (the reserved area, both FATs, the directories) and where each
//! file's data goes (one contiguous run of clusters); the caller writes them
//! and streams the files' contents to their offsets.
//!
//! Names are 8.3, upper case on disk; a base or extension written in lower
//! case is marked as such (the case bits Linux and Windows read), so
//! `loader.efi` reads back as `loader.efi`. Firmware compares names without
//! regard to case. No long names.

use std::collections::BTreeMap;

/// One file to put on the volume: its path (`/`-separated, no leading `/`)
/// and its size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSpec {
    pub path: String,
    pub size: u64,
}

/// Where everything goes, relative to the volume's start.
#[derive(Debug, Clone)]
pub struct Layout {
    pub sector: u64,
    pub cluster: u64,
    /// The reserved area: boot sector, FSInfo, their copies at 6 and 7. The
    /// volume is a FAT file system once these are written; write them last.
    pub reserved: (u64, Vec<u8>),
    /// Both FATs, then the directory clusters.
    pub tables: Vec<(u64, Vec<u8>)>,
    /// Each file's data offset, in the order given.
    pub files: Vec<(String, u64, u64)>,
    /// Bytes the files leave free.
    pub free_bytes: u64,
}

/// An 8.3 name: its 11 bytes and its case bits, or why it can't be one.
fn short(name: &str) -> Result<([u8; 11], u8), String> {
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) => (b, e),
        None => (name, ""),
    };
    let ok = |s: &str, max: usize| !s.is_empty() && s.len() <= max && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !ok(base, 8) || !(ext.is_empty() || ok(ext, 3)) {
        return Err(format!("{name:?} isn't an 8.3 name"));
    }
    let one_case = |s: &str| s.chars().all(|c| !c.is_ascii_lowercase()) || s.chars().all(|c| !c.is_ascii_uppercase());
    if !one_case(base) || !one_case(ext) {
        return Err(format!("{name:?} mixes upper and lower case"));
    }
    let mut out = [b' '; 11];
    for (i, c) in base.bytes().enumerate() {
        out[i] = c.to_ascii_uppercase();
    }
    for (i, c) in ext.bytes().enumerate() {
        out[8 + i] = c.to_ascii_uppercase();
    }
    let mut case = 0u8;
    if base.chars().any(|c| c.is_ascii_lowercase()) {
        case |= 0x08;
    }
    if ext.chars().any(|c| c.is_ascii_lowercase()) {
        case |= 0x10;
    }
    Ok((out, case))
}

/// 2026-01-01 00:00, the same on every build.
const DATE: u16 = ((2026 - 1980) << 9) | (1 << 5) | 1;

fn entry(name: [u8; 11], case: u8, dir: bool, cluster: u32, size: u32) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[0..11].copy_from_slice(&name);
    e[11] = if dir { 0x10 } else { 0x20 };
    e[12] = case;
    e[16..18].copy_from_slice(&DATE.to_le_bytes());
    e[18..20].copy_from_slice(&DATE.to_le_bytes());
    e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    e[24..26].copy_from_slice(&DATE.to_le_bytes());
    e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    e
}

#[derive(Default)]
struct Dir {
    /// name → (is a directory, index into files or dirs)
    children: BTreeMap<String, (bool, usize)>,
    cluster: u32,
    clusters: u32,
}

/// Lays out a FAT32 volume of `volume_bytes` holding `files`.
/// `hidden_sectors`: where the volume starts on its disk, in sectors (the
/// boot sector records it). `serial` and `label` (at most 11 characters)
/// name the volume.
pub fn layout(volume_bytes: u64, sector: u64, hidden_sectors: u64, serial: u32, label: &str, files: &[FileSpec]) -> Result<Layout, String> {
    if !(sector == 512 || sector == 1024 || sector == 2048 || sector == 4096) {
        return Err(format!("FAT32 with {sector}-byte sectors isn't written"));
    }
    let total = volume_bytes / sector;
    if total > u32::MAX as u64 {
        return Err("the volume is too large for FAT32".into());
    }
    // The cluster: 4 KiB, smaller where the volume would have too few
    // clusters for FAT32 (65525).
    let mut cluster = 4096u64.max(sector);
    while cluster > sector && volume_bytes / cluster < 66_000 {
        cluster /= 2;
    }
    let spc = cluster / sector;
    let reserved = (16384 / sector).max(8);
    let fat_sectors = ((total - reserved) / spc + 2).saturating_mul(4).div_ceil(sector);
    let data_start = reserved + 2 * fat_sectors;
    if data_start >= total {
        return Err("the volume is too small for FAT32".into());
    }
    let clusters = (total - data_start) / spc;
    if clusters < 65_525 {
        return Err(format!("a volume of {volume_bytes} bytes gives {clusters} clusters, too few for FAT32"));
    }

    // The directory tree.
    let mut dirs: Vec<Dir> = vec![Dir::default()];
    for (fi, f) in files.iter().enumerate() {
        let parts: Vec<&str> = f.path.split('/').collect();
        if parts.iter().any(|p| p.is_empty()) {
            return Err(format!("{:?} isn't a plain path", f.path));
        }
        let mut d = 0usize;
        for (k, p) in parts.iter().enumerate() {
            short(p)?;
            let key = p.to_ascii_uppercase();
            let last = k + 1 == parts.len();
            match dirs[d].children.get(&key).copied() {
                Some((true, sub)) if !last => d = sub,
                Some(_) => return Err(format!("{:?} names an existing path", f.path)),
                None if last => {
                    dirs[d].children.insert(key, (false, fi));
                }
                None => {
                    dirs.push(Dir::default());
                    let sub = dirs.len() - 1;
                    dirs[d].children.insert(key, (true, sub));
                    d = sub;
                }
            }
        }
        if f.size > u32::MAX as u64 {
            return Err(format!("{} is larger than a FAT32 file can be", f.path));
        }
    }
    // Names as given (case kept), by upper-case key.
    let mut given: BTreeMap<(usize, String), String> = BTreeMap::new();
    for f in files {
        let mut d = 0usize;
        for p in f.path.split('/') {
            let key = p.to_ascii_uppercase();
            given.entry((d, key.clone())).or_insert_with(|| p.to_string());
            if let Some((true, sub)) = dirs[d].children.get(&key).copied() {
                d = sub;
            }
        }
    }

    // Clusters: the directories first (root at 2), then each file's run.
    let per_cluster = (cluster / 32) as usize;
    let mut next = 2u32;
    for (i, d) in dirs.iter_mut().enumerate() {
        let n = d.children.len() + if i == 0 { 1 } else { 2 } + 1; // label or . and .., an end mark
        d.clusters = n.div_ceil(per_cluster) as u32;
        d.cluster = next;
        next += d.clusters;
    }
    let mut runs = vec![];
    for f in files {
        let n = f.size.div_ceil(cluster) as u32;
        runs.push((next, n));
        next += n;
    }
    let used = (next - 2) as u64;
    if used > clusters {
        return Err(format!("the files need {} bytes, the volume holds {}", used * cluster, clusters * cluster));
    }

    // The FAT.
    let mut fat = vec![0u8; (fat_sectors * sector) as usize];
    let mut set = |c: u32, v: u32| fat[c as usize * 4..c as usize * 4 + 4].copy_from_slice(&v.to_le_bytes());
    set(0, 0x0fff_fff8);
    set(1, 0x0fff_ffff);
    let chains = dirs.iter().map(|d| (d.cluster, d.clusters)).chain(runs.iter().copied());
    for (start, n) in chains {
        for k in 0..n {
            let c = start + k;
            set(c, if k + 1 == n { 0x0fff_ffff } else { c + 1 });
        }
    }

    // The directories.
    let off = |c: u32| (data_start + (c as u64 - 2) * spc) * sector;
    let mut tables = vec![(reserved * sector, fat.clone()), ((reserved + fat_sectors) * sector, fat)];
    let mut lab = [b' '; 11];
    let label_up = label.to_ascii_uppercase();
    if label_up.len() > 11 || !label_up.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        return Err(format!("{label:?} isn't a volume label"));
    }
    lab[..label_up.len()].copy_from_slice(label_up.as_bytes());
    let parent_of = |i: usize| -> u32 {
        for (p, d) in dirs.iter().enumerate() {
            if d.children.values().any(|(isdir, sub)| *isdir && *sub == i) {
                return if p == 0 { 0 } else { dirs[p].cluster };
            }
        }
        0
    };
    for (i, d) in dirs.iter().enumerate() {
        let mut b = vec![0u8; (d.clusters as u64 * cluster) as usize];
        let mut k = 0;
        let mut put = |e: [u8; 32]| {
            b[k * 32..k * 32 + 32].copy_from_slice(&e);
            k += 1;
        };
        if i == 0 {
            let mut e = [0u8; 32];
            e[0..11].copy_from_slice(&lab);
            e[11] = 0x08;
            e[24..26].copy_from_slice(&DATE.to_le_bytes());
            put(e);
        } else {
            put(entry(*b".          ", 0, true, d.cluster, 0));
            put(entry(*b"..         ", 0, true, parent_of(i), 0));
        }
        for (key, (isdir, idx)) in &d.children {
            let name = given.get(&(i, key.clone())).cloned().unwrap_or_else(|| key.clone());
            let (n, case) = short(&name)?;
            let (c, size) = if *isdir {
                (dirs[*idx].cluster, 0)
            } else {
                let (start, n) = runs[*idx];
                (if n == 0 { 0 } else { start }, files[*idx].size as u32)
            };
            put(entry(n, case, *isdir, c, size));
        }
        tables.push((off(d.cluster), b));
    }

    // The boot sector, FSInfo and their copies.
    let s = sector as usize;
    let mut r = vec![0u8; (reserved * sector) as usize];
    let mut bs = vec![0u8; s];
    bs[0..3].copy_from_slice(&[0xeb, 0x58, 0x90]);
    bs[3..11].copy_from_slice(b"GLASSINE");
    bs[11..13].copy_from_slice(&(sector as u16).to_le_bytes());
    bs[13] = spc as u8;
    bs[14..16].copy_from_slice(&(reserved as u16).to_le_bytes());
    bs[16] = 2;
    bs[21] = 0xf8;
    bs[24..26].copy_from_slice(&63u16.to_le_bytes());
    bs[26..28].copy_from_slice(&255u16.to_le_bytes());
    bs[28..32].copy_from_slice(&(hidden_sectors.min(u32::MAX as u64) as u32).to_le_bytes());
    bs[32..36].copy_from_slice(&(total as u32).to_le_bytes());
    bs[36..40].copy_from_slice(&(fat_sectors as u32).to_le_bytes());
    bs[44..48].copy_from_slice(&2u32.to_le_bytes());
    bs[48..50].copy_from_slice(&1u16.to_le_bytes());
    bs[50..52].copy_from_slice(&6u16.to_le_bytes());
    bs[64] = 0x80;
    bs[66] = 0x29;
    bs[67..71].copy_from_slice(&serial.to_le_bytes());
    bs[71..82].copy_from_slice(&lab);
    bs[82..90].copy_from_slice(b"FAT32   ");
    bs[510] = 0x55;
    bs[511] = 0xaa;
    let mut fsi = vec![0u8; s];
    fsi[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
    fsi[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
    fsi[488..492].copy_from_slice(&((clusters - used) as u32).to_le_bytes());
    fsi[492..496].copy_from_slice(&next.to_le_bytes());
    fsi[508..512].copy_from_slice(&0xaa55_0000u32.to_le_bytes());
    for (at, b) in [(0, &bs), (1, &fsi), (6, &bs), (7, &fsi)] {
        r[at * s..at * s + s].copy_from_slice(b);
    }
    let files_out = files.iter().zip(&runs).map(|(f, (start, _))| (f.path.clone(), off(*start), f.size)).collect();
    Ok(Layout {
        sector,
        cluster,
        reserved: (0, r),
        tables,
        files: files_out,
        free_bytes: (clusters - used) * cluster,
    })
}

pub mod read {
    //! A small FAT32 reader: what a firmware finds on a volume this module
    //! laid out (the tests' check, and a read-back of a whole volume).
    use crate::table::{le16, le32};

    /// The file at `path` of the volume `vol`, if it is there.
    pub fn file(vol: &[u8], path: &str) -> Option<Vec<u8>> {
        let bps = le16(vol, 11) as usize;
        let spc = vol[13] as usize;
        let reserved = le16(vol, 14) as usize;
        let fats = vol[16] as usize;
        let fatsz = le32(vol, 36) as usize;
        let root = le32(vol, 44);
        let data = (reserved + fats * fatsz) * bps;
        let cl = bps * spc;
        let fat = &vol[reserved * bps..(reserved + fatsz) * bps];
        let chain = |mut c: u32| {
            let mut out = vec![];
            while (2..0x0fff_fff8).contains(&c) {
                out.push(c);
                c = le32(fat, c as usize * 4) & 0x0fff_ffff;
            }
            out
        };
        let read = |c: u32| -> Vec<u8> { chain(c).iter().flat_map(|c| vol[data + (*c as usize - 2) * cl..][..cl].to_vec()).collect() };
        let mut dir = read(root);
        let parts: Vec<&str> = path.split('/').collect();
        for (k, p) in parts.iter().enumerate() {
            let (name, _) = super::short(p).ok()?;
            let e = dir.chunks(32).take_while(|e| e[0] != 0).find(|e| e[0..11] == name && e[11] & 0x08 == 0)?;
            let c = ((le16(e, 20) as u32) << 16) | le16(e, 26) as u32;
            if k + 1 == parts.len() {
                let size = le32(e, 28) as usize;
                let mut b = if c == 0 { vec![] } else { read(c) };
                b.truncate(size);
                return Some(b);
            }
            dir = read(c);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(volume: u64, sector: u64, files: &[(&str, Vec<u8>)]) -> (Vec<u8>, Layout) {
        let specs: Vec<FileSpec> = files
            .iter()
            .map(|(p, b)| FileSpec {
                path: p.to_string(),
                size: b.len() as u64,
            })
            .collect();
        let l = layout(volume, sector, 2048, 0x1234_5678, "GLASSINE", &specs).unwrap();
        let mut v = vec![0xa5u8; volume as usize]; // whatever was there before
        let mut put = |at: u64, b: &[u8]| v[at as usize..at as usize + b.len()].copy_from_slice(b);
        for (at, b) in &l.tables {
            put(*at, b);
        }
        for ((_, at, _), (_, b)) in l.files.iter().zip(files) {
            put(*at, b);
        }
        put(l.reserved.0, &l.reserved.1);
        (v, l)
    }

    #[test]
    fn files_read_back_at_their_paths() {
        let big: Vec<u8> = (0..100_000u32).map(|i| (i * 7) as u8).collect();
        let files = [
            ("EFI/BOOT/BOOTX64.EFI", b"loader".to_vec()),
            ("EFI/glassine/loader.efi", b"loader".to_vec()),
            ("glassine/loader.rec", b"format=1\n".to_vec()),
            ("glassine/0a1b2c3d/vmlinuz", big.clone()),
            ("glassine/0a1b2c3d/empty", vec![]),
        ];
        for sector in [512, 4096] {
            let (v, l) = build(300 << 20, sector, &files);
            for (p, b) in &files {
                assert_eq!(read::file(&v, p).as_deref(), Some(b.as_slice()), "{p} ({sector})");
            }
            assert_eq!(read::file(&v, "glassine/LOADER.REC").as_deref(), Some(&b"format=1\n"[..]));
            assert!(l.free_bytes > 290 << 20);
            // the same input, the same bytes
            let (w, _) = build(300 << 20, sector, &files);
            assert!(v == w);
        }
    }

    #[test]
    fn names_and_sizes_that_dont_fit_are_refused() {
        let f = |p: &str, size: u64| vec![FileSpec { path: p.into(), size }];
        assert!(layout(300 << 20, 512, 0, 1, "X", &f("glassine/toolongname.efi", 1)).is_err());
        assert!(layout(300 << 20, 512, 0, 1, "X", &f("glassine/Mixed.efi", 1)).is_err());
        assert!(layout(300 << 20, 512, 0, 1, "X", &f("/abs", 1)).is_err());
        assert!(layout(300 << 20, 512, 0, 1, "X", &f("a", 400 << 20)).unwrap_err().contains("the files need"));
        assert!(layout(16 << 20, 512, 0, 1, "X", &f("a", 1)).is_err());
        let two = vec![FileSpec { path: "a/b".into(), size: 1 }, FileSpec { path: "a".into(), size: 1 }];
        assert!(layout(300 << 20, 512, 0, 1, "X", &two).is_err());
    }
}
