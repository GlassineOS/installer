// SPDX-License-Identifier: GPL-2.0-or-later
//! A new GPT from the unit's own: the primary table as the verified backup
//! read it, changed by a layout recipe (renames, new partitions, the one
//! partition that shrinks to make room, a larger entry array), out as the
//! bytes of both copies with every CRC, and the list of what changed. Pure:
//! the caller writes the bytes (the backup copy first).
//!
//! The recipe names everything it touches. Every entry it doesn't name comes
//! out byte for byte as it was, in the same slot; a build that would differ
//! anywhere else is refused, and so is one where any partition would move.
//! The unique GUIDs of new partitions are inputs, not made here (the core
//! has no randomness): the edge draws them, a test states them.

use crate::table::{self, ReadAt, crc32, le32, le64};
use serde::{Deserialize, Serialize};
use std::fmt;

/// The entry size this builder writes and reads (every GPT seen in the wild).
pub const ENTRY_SIZE: usize = 128;
/// Name length in UTF-16 code units.
const NAME_UNITS: usize = 36;

/// What to change. Names refer to the source table (before renames).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    /// The logical sector size the table uses.
    pub sector: u64,
    /// Slots in the entry array after the change; never fewer than the source's.
    pub entries: u32,
    /// New partitions start (and end + 1) on multiples of this many sectors.
    pub align_sectors: u64,
    #[serde(default)]
    pub renames: Vec<Rename>,
    /// The partition whose end moves down to make room for the new ones.
    #[serde(default)]
    pub shrink: Option<String>,
    /// Where the new partitions go.
    #[serde(default)]
    pub place: Place,
    /// In disk order; they take the first empty slots, in this order.
    #[serde(default)]
    pub new: Vec<NewPartition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rename {
    pub from: String,
    pub to: String,
    /// New attribute bits (`0x…`); absent: kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attributes: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Place {
    /// Packed together, in recipe order, ending at the last aligned sector of
    /// the usable area (after the entry array grew); the shrinking partition
    /// ends right before the first of them.
    #[default]
    EndOfDisk,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewPartition {
    pub name: String,
    pub size: Size,
    #[serde(rename = "type")]
    pub type_guid: TypeOf,
    /// Attribute bits, `0x…`.
    pub attributes: String,
    /// The unique partition GUID (text form). Drawn by the caller.
    pub guid: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Size {
    /// The size of a source partition.
    Like(String),
    Bytes(u64),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TypeOf {
    /// The type GUID of a source partition.
    Like(String),
    Guid(String),
}

/// One change, for the person and the install record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "change", rename_all = "kebab-case")]
pub enum Change {
    Renamed {
        slot: u32,
        from: String,
        to: String,
        attributes_before: String,
        attributes_after: String,
    },
    Shrunk {
        slot: u32,
        name: String,
        last_before: u64,
        last_after: u64,
        freed_bytes: u64,
    },
    Added {
        slot: u32,
        name: String,
        first: u64,
        last: u64,
        size_bytes: u64,
        type_guid: String,
        attributes: String,
        guid: String,
    },
    EntryArray {
        entries_before: u32,
        entries_after: u32,
        last_usable_before: u64,
        last_usable_after: u64,
    },
}

impl fmt::Display for Change {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Change::Renamed {
                slot,
                from,
                to,
                attributes_before,
                attributes_after,
            } => {
                write!(f, "partition {slot}: {from} is renamed {to}")?;
                if attributes_before != attributes_after {
                    write!(f, ", attributes {attributes_before} -> {attributes_after}")?;
                }
                Ok(())
            }
            Change::Shrunk {
                slot,
                name,
                last_before,
                last_after,
                freed_bytes,
            } => write!(
                f,
                "partition {slot}: {name} ends at sector {last_after} instead of {last_before} ({freed_bytes} bytes smaller; what is on it is lost)"
            ),
            Change::Added {
                slot,
                name,
                first,
                last,
                size_bytes,
                attributes,
                ..
            } => write!(
                f,
                "partition {slot}: new {name}, sectors {first}..{last} ({size_bytes} bytes), attributes {attributes}"
            ),
            Change::EntryArray {
                entries_before,
                entries_after,
                last_usable_before,
                last_usable_after,
            } => write!(
                f,
                "the table holds {entries_after} entries instead of {entries_before}; the last usable sector is {last_usable_after} instead of {last_usable_before}"
            ),
        }
    }
}

/// The two copies of the new table, ready to write, and what changed.
#[derive(Debug, Clone)]
pub struct Built {
    pub sector: u64,
    pub disk_sectors: u64,
    /// Protective MBR, primary header, entry array: written at sector 0.
    pub primary: Vec<u8>,
    /// Entry array, then the backup header: written at `backup_sector`
    /// (it ends on the disk's last sector).
    pub backup: Vec<u8>,
    pub backup_sector: u64,
    pub changes: Vec<Change>,
}

impl Built {
    /// Byte offsets: where each copy goes.
    pub fn primary_offset(&self) -> u64 {
        0
    }
    pub fn backup_offset(&self) -> u64 {
        self.backup_sector * self.sector
    }
    /// The changes as lines for the person.
    pub fn describe(&self) -> Vec<String> {
        self.changes.iter().map(|c| c.to_string()).collect()
    }
}

/// Attribute bits from `0x…` (or decimal).
pub fn parse_attributes(s: &str) -> Result<u64, String> {
    let t = s.trim();
    let r = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16),
        None => t.parse(),
    };
    r.map_err(|_| format!("'{s}' is not an attribute value"))
}

fn attr_text(a: u64) -> String {
    format!("0x{a:016x}")
}

/// The on-disk (mixed-endian) bytes of a GUID in its text form.
pub fn guid_bytes(s: &str) -> Result<[u8; 16], String> {
    let bad = || format!("'{s}' is not a GUID");
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 || [8, 4, 4, 4, 12].iter().zip(&parts).any(|(n, p)| p.len() != *n) {
        return Err(bad());
    }
    let hex: String = parts.concat();
    let mut raw = [0u8; 16];
    for (i, b) in raw.iter_mut().enumerate() {
        *b = u8::from_str_radix(hex.get(i * 2..i * 2 + 2).ok_or_else(bad)?, 16).map_err(|_| bad())?;
    }
    let mut out = raw;
    out[0..4].copy_from_slice(&[raw[3], raw[2], raw[1], raw[0]]);
    out[4..6].copy_from_slice(&[raw[5], raw[4]]);
    out[6..8].copy_from_slice(&[raw[7], raw[6]]);
    Ok(out)
}

/// A new partition resolved: its recipe entry, sectors, type GUID, unique GUID, attributes.
type NewSpec<'a> = (&'a NewPartition, u64, [u8; 16], [u8; 16], u64);

/// A slot of the entry array, as raw bytes plus the fields we use.
#[derive(Clone)]
struct Slot {
    raw: [u8; ENTRY_SIZE],
}

impl Slot {
    fn empty(&self) -> bool {
        self.raw[0..16].iter().all(|b| *b == 0)
    }
    fn first(&self) -> u64 {
        le64(&self.raw, 32)
    }
    fn last(&self) -> u64 {
        le64(&self.raw, 40)
    }
    fn attrs(&self) -> u64 {
        le64(&self.raw, 48)
    }
    fn name(&self) -> String {
        String::from_utf16_lossy(&table::utf16le(&self.raw[56..128]))
    }
    fn set_last(&mut self, v: u64) {
        self.raw[40..48].copy_from_slice(&v.to_le_bytes());
    }
    fn set_attrs(&mut self, v: u64) {
        self.raw[48..56].copy_from_slice(&v.to_le_bytes());
    }
    fn set_name(&mut self, n: &str) -> Result<(), String> {
        let units: Vec<u16> = n.encode_utf16().collect();
        if units.is_empty() || units.len() > NAME_UNITS {
            return Err(format!("the name '{n}' doesn't fit a GPT entry (1 to {NAME_UNITS} characters)"));
        }
        self.raw[56..128].fill(0);
        for (i, u) in units.iter().enumerate() {
            self.raw[56 + i * 2..58 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        Ok(())
    }
}

/// What both headers share; each copy adds where it is and where the other is.
struct Header<'a> {
    sector: u64,
    first_usable: u64,
    last_usable: u64,
    disk_guid: &'a [u8],
    entries: u32,
    array_crc: u32,
}

fn header(g: &Header, current: u64, alternate: u64, array_at: u64) -> Vec<u8> {
    let Header {
        sector,
        first_usable,
        last_usable,
        disk_guid,
        entries,
        array_crc,
    } = *g;
    let mut h = vec![0u8; sector as usize];
    h[0..8].copy_from_slice(b"EFI PART");
    h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
    h[12..16].copy_from_slice(&92u32.to_le_bytes());
    h[24..32].copy_from_slice(&current.to_le_bytes());
    h[32..40].copy_from_slice(&alternate.to_le_bytes());
    h[40..48].copy_from_slice(&first_usable.to_le_bytes());
    h[48..56].copy_from_slice(&last_usable.to_le_bytes());
    h[56..72].copy_from_slice(disk_guid);
    h[72..80].copy_from_slice(&array_at.to_le_bytes());
    h[80..84].copy_from_slice(&entries.to_le_bytes());
    h[84..88].copy_from_slice(&(ENTRY_SIZE as u32).to_le_bytes());
    h[88..92].copy_from_slice(&array_crc.to_le_bytes());
    let c = crc32(&h[0..92]);
    h[16..20].copy_from_slice(&c.to_le_bytes());
    h
}

/// Builds the new table. `source` is the primary table as read off the
/// unit: sector 0 (protective MBR), the header at sector 1, and the whole
/// entry array after it. `disk_bytes`, when known, must agree with where the
/// source's header puts its backup copy.
pub fn build(source: &[u8], disk_bytes: Option<u64>, recipe: &Recipe) -> Result<Built, String> {
    let s = recipe.sector;
    if !(512..=4096).contains(&s) || !s.is_power_of_two() {
        return Err(format!("sector size {s} is not one a GPT uses"));
    }
    let su = s as usize;
    if source.len() < 2 * su {
        return Err("the source is shorter than a protective MBR and a GPT header".into());
    }
    let h = &source[su..2 * su];
    if &h[0..8] != b"EFI PART" {
        return Err("the source has no GPT header at sector 1".into());
    }
    if le32(h, 12) != 92 {
        return Err(format!("the source's header is {} bytes; this builder knows 92", le32(h, 12)));
    }
    let mut hc = h[0..92].to_vec();
    hc[16..20].fill(0);
    if crc32(&hc) != le32(h, 16) {
        return Err("the source's header CRC is wrong: refusing to build from a damaged table".into());
    }
    if le64(h, 24) != 1 {
        return Err("the source is not a primary header (its own sector isn't 1)".into());
    }
    let (alternate, first_usable, last_usable_before) = (le64(h, 32), le64(h, 40), le64(h, 48));
    let disk_guid = h[56..72].to_vec();
    let (src_array_at, src_n, esize) = (le64(h, 72), le32(h, 80), le32(h, 84) as usize);
    if esize != ENTRY_SIZE {
        return Err(format!("the source's entries are {esize} bytes; this builder knows {ENTRY_SIZE}"));
    }
    if src_array_at != 2 {
        return Err(format!("the source's entry array starts at sector {src_array_at}, not 2"));
    }
    let array_bytes = src_n as usize * ENTRY_SIZE;
    let src_array = source.get(2 * su..2 * su + array_bytes).ok_or("the source is shorter than its entry array")?;
    if crc32(src_array) != le32(h, 88) {
        return Err("the source's entry array CRC is wrong: refusing to build from a damaged table".into());
    }
    if first_usable > last_usable_before || last_usable_before >= alternate || alternate == u64::MAX {
        return Err("the source's header geometry doesn't hold together".into());
    }
    let disk_sectors = alternate + 1;
    if let Some(b) = disk_bytes
        && b / s != disk_sectors
    {
        return Err(format!(
            "the disk has {} sectors but the source table's backup header is at {alternate}: not this disk's table",
            b / s
        ));
    }
    if recipe.entries < src_n {
        return Err(format!("the recipe has {} entries, fewer than the source's {src_n}", recipe.entries));
    }
    if recipe.align_sectors == 0 {
        return Err("alignment must be at least one sector".into());
    }

    let src: Vec<Slot> = (0..src_n as usize)
        .map(|i| Slot {
            raw: src_array[i * ENTRY_SIZE..(i + 1) * ENTRY_SIZE].try_into().unwrap(),
        })
        .collect();
    let find = |name: &str| -> Result<usize, String> {
        let hits: Vec<usize> = (0..src.len()).filter(|i| !src[*i].empty() && src[*i].name() == name).collect();
        match hits.as_slice() {
            [i] => Ok(*i),
            [] => Err(format!("the table has no partition {name}")),
            _ => Err(format!("the table has {} partitions named {name}", hits.len())),
        }
    };
    let mut slots = src.clone();
    slots.resize(recipe.entries as usize, Slot { raw: [0; ENTRY_SIZE] });
    let mut named: Vec<usize> = vec![];
    let mut changes = vec![];

    // Geometry after the array grows.
    let array_sectors = (recipe.entries as u64 * ENTRY_SIZE as u64).div_ceil(s);
    if 2 + array_sectors > first_usable {
        return Err(format!(
            "an array of {} entries reaches sector {}, past the first usable sector {first_usable}",
            recipe.entries,
            1 + array_sectors
        ));
    }
    let backup_header = disk_sectors - 1;
    // Recomputed from the geometry: the sector before the backup array.
    let last_usable = backup_header
        .checked_sub(array_sectors + 1)
        .filter(|l| *l >= first_usable)
        .ok_or("the disk is too small for the entry array")?;
    let backup_array = last_usable + 1;

    for r in &recipe.renames {
        let i = find(&r.from)?;
        if named.contains(&i) {
            return Err(format!("{} is renamed twice", r.from));
        }
        named.push(i);
        let before = slots[i].attrs();
        slots[i].set_name(&r.to)?;
        if let Some(a) = &r.attributes {
            slots[i].set_attrs(parse_attributes(a)?);
        }
        changes.push(Change::Renamed {
            slot: i as u32 + 1,
            from: r.from.clone(),
            to: r.to.clone(),
            attributes_before: attr_text(before),
            attributes_after: attr_text(slots[i].attrs()),
        });
    }

    // New partitions: sizes and types from the source, packed at the end.
    let mut new_specs: Vec<NewSpec> = vec![];
    for n in &recipe.new {
        let sectors = match &n.size {
            Size::Like(p) => {
                let x = &src[find(p)?];
                x.last() - x.first() + 1
            }
            Size::Bytes(b) => {
                if b % s != 0 || *b == 0 {
                    return Err(format!("{}: {b} bytes is not a whole number of sectors", n.name));
                }
                b / s
            }
        };
        if sectors % recipe.align_sectors != 0 {
            return Err(format!("{}: {sectors} sectors is not a multiple of the alignment", n.name));
        }
        let type_guid: [u8; 16] = match &n.type_guid {
            TypeOf::Like(p) => src[find(p)?].raw[0..16].try_into().unwrap(),
            TypeOf::Guid(g) => guid_bytes(g)?,
        };
        if type_guid == [0; 16] {
            return Err(format!("{}: the zero type GUID marks an empty slot", n.name));
        }
        let guid = guid_bytes(&n.guid)?;
        if guid == [0; 16] {
            return Err(format!("{}: its unique GUID is zero", n.name));
        }
        if src.iter().any(|x| !x.empty() && x.raw[16..32] == guid) || new_specs.iter().any(|x| x.3 == guid) {
            return Err(format!("{}: its unique GUID {} is already in the table", n.name, n.guid));
        }
        new_specs.push((n, sectors, type_guid, guid, parse_attributes(&n.attributes)?));
    }
    let total = new_specs
        .iter()
        .try_fold(0u64, |t, x| t.checked_add(x.1))
        .ok_or("the new partitions are larger than the disk")?;
    let end = (last_usable + 1) / recipe.align_sectors * recipe.align_sectors;
    let first_new = end.checked_sub(total).ok_or("the new partitions are larger than the disk")?;

    if let Some(name) = &recipe.shrink {
        let i = find(name)?;
        if !named.contains(&i) {
            named.push(i);
        }
        let before = slots[i].last();
        if new_specs.is_empty() {
            return Err(format!("{name} would shrink with nothing to make room for"));
        }
        let after = first_new.checked_sub(1).ok_or("the new partitions are larger than the disk")?;
        if after >= before {
            return Err(format!("{name} ends at {before}, before the new partitions start: it wouldn't shrink"));
        }
        if after < slots[i].first() {
            return Err(format!("{name} is too small to give up {total} sectors"));
        }
        slots[i].set_last(after);
        changes.push(Change::Shrunk {
            slot: i as u32 + 1,
            name: slots[i].name(),
            last_before: before,
            last_after: after,
            freed_bytes: (before - after) * s,
        });
    } else if !new_specs.is_empty() {
        return Err("new partitions need a partition that shrinks to make room".into());
    }

    let mut at = first_new;
    for (n, sectors, type_guid, guid, attrs) in new_specs {
        let i = slots
            .iter()
            .position(|x| x.empty())
            .ok_or_else(|| format!("no empty slot left for {}", n.name))?;
        let mut e = Slot { raw: [0; ENTRY_SIZE] };
        e.raw[0..16].copy_from_slice(&type_guid);
        e.raw[16..32].copy_from_slice(&guid);
        e.raw[32..40].copy_from_slice(&at.to_le_bytes());
        e.raw[40..48].copy_from_slice(&(at + sectors - 1).to_le_bytes());
        e.set_attrs(attrs);
        e.set_name(&n.name)?;
        slots[i] = e;
        named.push(i);
        changes.push(Change::Added {
            slot: i as u32 + 1,
            name: n.name.clone(),
            first: at,
            last: at + sectors - 1,
            size_bytes: sectors * s,
            type_guid: table::guid(&type_guid),
            attributes: attr_text(attrs),
            guid: n.guid.to_lowercase(),
        });
        at += sectors;
    }
    if recipe.entries != src_n || last_usable != last_usable_before {
        changes.push(Change::EntryArray {
            entries_before: src_n,
            entries_after: recipe.entries,
            last_usable_before,
            last_usable_after: last_usable,
        });
    }

    check(&src, &slots, &named, first_usable, last_usable)?;

    let mut array: Vec<u8> = slots.iter().flat_map(|x| x.raw).collect();
    let array_crc = crc32(&array);
    array.resize((array_sectors * s) as usize, 0);
    let mut primary = source[0..su].to_vec();
    let g = Header {
        sector: s,
        first_usable,
        last_usable,
        disk_guid: &disk_guid,
        entries: recipe.entries,
        array_crc,
    };
    primary.extend(header(&g, 1, backup_header, 2));
    primary.extend(&array);
    let mut backup = array;
    backup.extend(header(&g, backup_header, 1, backup_array));
    let built = Built {
        sector: s,
        disk_sectors,
        primary,
        backup,
        backup_sector: backup_array,
        changes,
    };
    reread(&built, &slots)?;
    Ok(built)
}

/// The invariants: unnamed slots unchanged byte for byte, no partition moved
/// its start, names unique, nothing overlaps, all inside the usable area.
fn check(src: &[Slot], out: &[Slot], named: &[usize], first: u64, last: u64) -> Result<(), String> {
    for (i, a) in src.iter().enumerate() {
        if !named.contains(&i) && a.raw != out[i].raw {
            return Err(format!("slot {} would change though the recipe doesn't name it", i + 1));
        }
        if !a.empty() && (out[i].empty() || out[i].first() != a.first()) {
            return Err(format!("{} would move: refused", a.name()));
        }
    }
    let mut used: Vec<(u64, u64, String)> = out.iter().filter(|x| !x.empty()).map(|x| (x.first(), x.last(), x.name())).collect();
    for (f, l, n) in &used {
        if f > l || *f < first || *l > last {
            return Err(format!("{n} ({f}..{l}) is outside the usable sectors {first}..{last}"));
        }
    }
    used.sort();
    for w in used.windows(2) {
        if w[0].1 >= w[1].0 {
            return Err(format!("{} and {} would overlap", w[0].2, w[1].2));
        }
    }
    let mut names: Vec<&String> = used.iter().map(|x| &x.2).collect();
    names.sort();
    if let Some(w) = names.windows(2).find(|w| w[0] == w[1]) {
        return Err(format!("two partitions would be named {}", w[0]));
    }
    Ok(())
}

/// Both copies parsed back by the table reader, each alone, give the slots.
fn reread(b: &Built, slots: &[Slot]) -> Result<(), String> {
    struct Sparse<'a> {
        parts: Vec<(u64, &'a [u8])>,
    }
    impl ReadAt for Sparse<'_> {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
            buf.fill(0);
            for (at, bytes) in &self.parts {
                let (lo, hi) = (offset.max(*at), (offset + buf.len() as u64).min(at + bytes.len() as u64));
                if lo < hi {
                    buf[(lo - offset) as usize..(hi - offset) as usize].copy_from_slice(&bytes[(lo - at) as usize..(hi - at) as usize]);
                }
            }
            Ok(())
        }
    }
    let size = b.disk_sectors * b.sector;
    let want: Vec<(u64, u64, Option<String>)> = slots
        .iter()
        .filter(|x| !x.empty())
        .map(|x| (x.first() * b.sector, (x.last() - x.first() + 1) * b.sector, Some(x.name())))
        .collect();
    // A copy of the protective MBR only, so the reader falls back to the backup.
    let mbr = &b.primary[..b.sector as usize];
    for (what, parts, primary) in [
        ("primary", vec![(0, b.primary.as_slice())], true),
        ("backup", vec![(0, mbr), (b.backup_offset(), b.backup.as_slice())], false),
    ] {
        let t = table::read(&mut Sparse { parts }, size, b.sector)?;
        let got: Vec<(u64, u64, Option<String>)> = t.entries.iter().map(|e| (e.start, e.size, e.name.clone())).collect();
        if t.kind != table::Kind::Gpt || t.primary_ok != primary || got != want {
            return Err(format!("the built {what} copy doesn't read back as built"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_text_round_trips() {
        let t = "20117f86-e985-4357-b9ee-374bc1d8487d";
        assert_eq!(table::guid(&guid_bytes(t).unwrap()), t);
        assert!(guid_bytes("20117f86-e985-4357-b9ee-374bc1d8487").is_err());
        assert!(guid_bytes("2011zf86-e985-4357-b9ee-374bc1d8487d").is_err());
    }

    #[test]
    fn attributes_parse() {
        assert_eq!(parse_attributes("0x007F000000000000").unwrap(), 0x007f_0000_0000_0000);
        assert_eq!(parse_attributes("12").unwrap(), 12);
        assert!(parse_attributes("0xzz").is_err());
    }
}
