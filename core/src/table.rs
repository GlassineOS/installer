// SPDX-License-Identifier: GPL-2.0-or-later
//! Partition tables, parsed by our own code from bytes read off a disk or an
//! image file: GPT (primary header, else the backup) and MBR (primary entries
//! and the extended partition's logical ones). Nothing here writes; a reader
//! only reads, at offsets the parser picks. The unallocated regions come out
//! too, aligned to MiB, as a partitioner would offer them.

use std::collections::BTreeSet;

/// Bytes at an offset of a disk or an image. The only thing a parser here
/// can do with a disk.
pub trait ReadAt {
    /// Fills `buf` from `offset`, or fails: a short read is an error.
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String>;
}

/// A disk held in memory (tests, and images small enough to read whole).
impl ReadAt for &[u8] {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let start = usize::try_from(offset).map_err(|_| "offset past the end".to_string())?;
        let end = start.checked_add(buf.len()).ok_or("offset past the end")?;
        let src = self
            .get(start..end)
            .ok_or_else(|| format!("read of {} bytes at {offset} is past the end", buf.len()))?;
        buf.copy_from_slice(src);
        Ok(())
    }
}

/// A region of a disk, relative to a base offset: what the file-system probes
/// read through, so they can't read outside their partition.
pub struct Window<'a> {
    pub disk: &'a mut dyn ReadAt,
    pub start: u64,
    pub size: u64,
}

impl ReadAt for Window<'_> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let end = offset.checked_add(buf.len() as u64).ok_or("offset overflows")?;
        if end > self.size {
            return Err(format!("read of {} bytes at {offset} is past the partition's end ({})", buf.len(), self.size));
        }
        self.disk.read_at(self.start + offset, buf)
    }
}

/// Reads `len` bytes at `offset`.
pub fn read_vec(r: &mut dyn ReadAt, offset: u64, len: usize) -> Result<Vec<u8>, String> {
    let mut v = vec![0u8; len];
    r.read_at(offset, &mut v)?;
    Ok(v)
}

pub const MIB: u64 = 1 << 20;
pub const ESP_TYPE: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Gpt,
    Mbr,
    /// No partition table: a blank disk, or a file system on the whole disk.
    None,
    /// A table is announced (a protective MBR, a GPT signature) but neither
    /// copy checks out. Nothing on such a disk may be written.
    Unreadable,
}

impl Kind {
    /// The facts' name for it; `None` for no table.
    pub fn name(&self) -> Option<&'static str> {
        match self {
            Kind::Gpt => Some("gpt"),
            Kind::Mbr => Some("mbr"),
            Kind::None => None,
            Kind::Unreadable => Some("unreadable"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The number Linux gives it: the GPT slot, or the MBR slot (5 and up for
    /// logical partitions).
    pub index: u32,
    pub start: u64,
    pub size: u64,
    /// A GPT type GUID (lower case), or an MBR type as `0x0c`.
    pub type_id: String,
    /// The GPT partition name. A label a person may have chosen.
    pub name: Option<String>,
    /// The GPT unique partition GUID. Unique to the unit: kept off the facts,
    /// used only to match a firmware boot entry to its disk.
    pub guid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Table {
    pub kind: Kind,
    /// The logical sector size the table was found with.
    pub sector: u64,
    pub entries: Vec<Entry>,
    /// Unallocated regions as (start, length), each at least 1 MiB, aligned.
    pub free: Vec<(u64, u64)>,
    /// Which GPT copy was read: false when the primary was damaged and the
    /// backup was used.
    pub primary_ok: bool,
}

/// The CRC-32 GPT uses (IEEE, reflected).
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xffff_ffffu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xedb8_8320 } else { c >> 1 };
        }
    }
    !c
}

pub fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
pub fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
pub fn le64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// A GUID in its text form from its on-disk (mixed-endian) bytes.
pub fn guid(b: &[u8]) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        le32(b, 0),
        le16(b, 4),
        le16(b, 6),
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    )
}

/// UTF-16LE code units up to the first NUL.
pub fn utf16le(b: &[u8]) -> Vec<u16> {
    b.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).take_while(|u| *u != 0).collect()
}

fn utf16z(b: &[u8]) -> String {
    String::from_utf16_lossy(&utf16le(b))
}

/// The partition table of a disk of `size` bytes whose logical sector size
/// the kernel reports as `sector` (0 when unknown: 512 and 4096 are tried).
pub fn read(r: &mut dyn ReadAt, size: u64, sector: u64) -> Result<Table, String> {
    let mut sizes: Vec<u64> = vec![];
    for s in [sector, 512, 4096] {
        if s >= 512 && s.is_power_of_two() && !sizes.contains(&s) {
            sizes.push(s);
        }
    }
    let lba0 = read_vec(r, 0, 512)?;
    let mbr_sig = lba0[510] == 0x55 && lba0[511] == 0xaa;
    let protective = mbr_sig && (0..4).any(|i| lba0[446 + i * 16 + 4] == 0xee);
    let mut announced = protective;
    for &s in &sizes {
        if size < s * 34 {
            continue;
        }
        match gpt_at(r, size, s, s)? {
            GptRead::Valid(t) => return Ok(t),
            GptRead::Damaged => announced = true,
            GptRead::Absent => {}
        }
        // The primary is absent or damaged: the backup header, at the last LBA.
        if let GptRead::Valid(mut t) = gpt_at(r, size, s, size / s * s - s)? {
            t.primary_ok = false;
            return Ok(t);
        }
    }
    if announced {
        return Ok(Table {
            kind: Kind::Unreadable,
            sector: sizes[0],
            entries: vec![],
            free: vec![],
            primary_ok: false,
        });
    }
    if mbr_sig && let Some(t) = mbr(r, &lba0, size)? {
        return Ok(t);
    }
    // No table: blank, or a file system on the whole disk (the caller probes).
    let s = sizes[0];
    let end = size.saturating_sub(33 * s);
    Ok(Table {
        kind: Kind::None,
        sector: s,
        entries: vec![],
        free: free_regions(MIB, end, &[]),
        primary_ok: true,
    })
}

enum GptRead {
    Valid(Table),
    /// A signature, but the header or the entries don't check out.
    Damaged,
    Absent,
}

fn gpt_at(r: &mut dyn ReadAt, size: u64, s: u64, at: u64) -> Result<GptRead, String> {
    let h = read_vec(r, at, 512)?;
    if &h[0..8] != b"EFI PART" {
        return Ok(GptRead::Absent);
    }
    let hsize = le32(&h, 12) as usize;
    if !(92..=512).contains(&hsize) {
        return Ok(GptRead::Damaged);
    }
    let mut hc = h[..hsize].to_vec();
    hc[16..20].fill(0);
    if crc32(&hc) != le32(&h, 16) || le64(&h, 24) != at / s {
        return Ok(GptRead::Damaged);
    }
    let (first, last) = (le64(&h, 40), le64(&h, 48));
    let (elba, n, esize) = (le64(&h, 72), le32(&h, 80) as u64, le32(&h, 84) as u64);
    let lbas = size / s;
    if !(128..=4096).contains(&esize) || n == 0 || n > 4096 || first > last || last >= lbas || elba >= lbas {
        return Ok(GptRead::Damaged);
    }
    let entries = match read_vec(r, elba * s, (n * esize) as usize) {
        Ok(e) => e,
        Err(_) => return Ok(GptRead::Damaged),
    };
    if crc32(&entries) != le32(&h, 88) {
        return Ok(GptRead::Damaged);
    }
    let mut out = Vec::new();
    for i in 0..n as usize {
        let e = &entries[i * esize as usize..(i + 1) * esize as usize];
        if e[0..16].iter().all(|b| *b == 0) {
            continue;
        }
        let (fl, ll) = (le64(e, 32), le64(e, 40));
        if ll < fl || ll >= lbas {
            return Ok(GptRead::Damaged);
        }
        let name = utf16z(&e[56..128]);
        out.push(Entry {
            index: i as u32 + 1,
            start: fl * s,
            size: (ll - fl + 1) * s,
            type_id: guid(&e[0..16]),
            name: (!name.is_empty()).then_some(name),
            guid: Some(guid(&e[16..32])),
        });
    }
    let used: Vec<(u64, u64)> = out.iter().map(|e| (e.start, e.size)).collect();
    Ok(GptRead::Valid(Table {
        kind: Kind::Gpt,
        sector: s,
        free: free_regions(first * s, (last + 1) * s, &used),
        entries: out,
        primary_ok: true,
    }))
}

fn mbr(r: &mut dyn ReadAt, lba0: &[u8], size: u64) -> Result<Option<Table>, String> {
    let s = 512u64;
    let mut out = Vec::new();
    let mut extended: Option<(u64, u64)> = None;
    for i in 0..4 {
        let e = &lba0[446 + i * 16..446 + (i + 1) * 16];
        if e[0] != 0 && e[0] != 0x80 {
            return Ok(None); // not a partition entry: a boot sector of a file system
        }
        let (ty, start, len) = (e[4], le32(e, 8) as u64, le32(e, 12) as u64);
        if ty == 0 || len == 0 {
            continue;
        }
        if start == 0 || (start + len) * s > size {
            return Ok(None);
        }
        out.push(Entry {
            index: i as u32 + 1,
            start: start * s,
            size: len * s,
            type_id: format!("0x{ty:02x}"),
            name: None,
            guid: None,
        });
        if matches!(ty, 0x05 | 0x0f | 0x85) {
            extended = Some((start, len));
        }
    }
    // Logical partitions: the chain of extended boot records.
    if let Some((ext, ext_len)) = extended {
        let mut at = ext;
        let mut seen = BTreeSet::new();
        let mut index = 5;
        while seen.insert(at) && seen.len() <= 128 {
            let b = read_vec(r, at * s, 512)?;
            if b[510] != 0x55 || b[511] != 0xaa {
                break;
            }
            let (ty, rel, len) = (b[446 + 4], le32(&b, 446 + 8) as u64, le32(&b, 446 + 12) as u64);
            if ty != 0 && len != 0 && at + rel + len <= ext + ext_len {
                out.push(Entry {
                    index,
                    start: (at + rel) * s,
                    size: len * s,
                    type_id: format!("0x{ty:02x}"),
                    name: None,
                    guid: None,
                });
                index += 1;
            }
            let next = le32(&b, 446 + 16 + 8) as u64;
            if b[446 + 16 + 4] == 0 || next == 0 {
                break;
            }
            at = ext + next;
        }
    }
    if out.is_empty() {
        // A signature and no entry: a file system's boot sector, or an empty table.
        return Ok(None);
    }
    let used: Vec<(u64, u64)> = out.iter().filter(|e| e.index <= 4).map(|e| (e.start, e.size)).collect();
    Ok(Some(Table {
        kind: Kind::Mbr,
        sector: s,
        free: free_regions(MIB.min(size), size, &used),
        entries: out,
        primary_ok: true,
    }))
}

/// The regions of [first, end) no `used` (start, length) covers, each
/// aligned to MiB and at least 1 MiB long.
pub fn free_regions(first: u64, end: u64, used: &[(u64, u64)]) -> Vec<(u64, u64)> {
    let mut u: Vec<(u64, u64)> = used.iter().map(|(s, l)| (*s, s + l)).collect();
    u.sort();
    let mut out = Vec::new();
    let mut at = first;
    let mut push = |a: u64, b: u64| {
        let a = a.div_ceil(MIB) * MIB;
        let b = b / MIB * MIB;
        if b > a && b - a >= MIB {
            out.push((a, b - a));
        }
    };
    for (s, e) in u {
        if s > at {
            push(at, s.min(end));
        }
        at = at.max(e);
    }
    if end > at {
        push(at, end);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_of_a_known_string() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn free_regions_are_aligned_and_skip_slivers() {
        let r = free_regions(17408, 100 * MIB, &[(MIB, 10 * MIB), (11 * MIB + 4096, MIB / 2)]);
        // [17408, 1 MiB) is a sliver; after the first partition up to the
        // second: [11 MiB, 11 MiB + 4 KiB) a sliver; from the end of the second
        // (rounded up to 12 MiB) to 100 MiB.
        assert_eq!(r, vec![(12 * MIB, 88 * MIB)]);
    }
}
