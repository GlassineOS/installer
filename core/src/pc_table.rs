// SPDX-License-Identifier: GPL-2.0-or-later
//! A PC disk's new GPT (installer 001 FR-012a): the disk's table as read
//! before the first write, less the partitions of the systems the plan
//! replaces, plus GlassineOS's partitions in the largest free region. Out as
//! the bytes of both copies with every CRC, and the list of what changed.
//! Pure: the caller writes the bytes (the backup copy first, then the
//! primary).
//!
//! Every entry the build doesn't remove comes out byte for byte as it was,
//! in the same slot, and so does sector 0 (the protective MBR) and the disk's
//! GUID. A disk with no table gets a new one: a protective MBR, 128 entries.
//! A disk with an MBR table, a damaged GPT, or a GPT of another entry size is
//! refused. The unique GUIDs (of new partitions, of a new disk) are inputs:
//! the core has no randomness.

use crate::gpt_build::{Built, Change, ENTRY_SIZE, Header, guid_bytes, header};
use crate::table::{MIB, crc32, guid, le32, le64};

/// The entries a new table has.
pub const NEW_ENTRIES: u32 = 128;

/// One partition to add.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPart {
    pub name: String,
    pub size: NewSize,
    /// The type GUID, text form.
    pub type_guid: String,
    /// The unique partition GUID, text form.
    pub guid: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewSize {
    Bytes(u64),
    /// What is left of the region, at least this many bytes.
    Rest {
        at_least: u64,
    },
}

/// What the build starts from: the disk's first sectors as read before the
/// first write (sector 0 to the end of the primary entry array, or at least
/// two sectors when there is no table) and the disk's size.
pub struct Source<'a> {
    pub head: &'a [u8],
    pub disk_bytes: u64,
    pub sector: u64,
}

/// Where a new partition landed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placed {
    pub slot: u32,
    pub name: String,
    pub first: u64,
    pub last: u64,
    pub guid: String,
}

/// The built table and where each new partition is.
#[derive(Debug, Clone)]
pub struct PcTable {
    pub built: Built,
    pub placed: Vec<Placed>,
    /// The disk's GUID, text form.
    pub disk_guid: String,
}

/// The length of a disk's primary table region from its first sectors: the
/// protective MBR, the header and the entry array; two sectors when it has
/// no GPT.
pub fn head_len(head: &[u8], sector: u64) -> u64 {
    let s = sector as usize;
    match head.get(s..2 * s) {
        Some(h) if &h[0..8] == b"EFI PART" => {
            let (at, n, esize) = (le64(h, 72), le32(h, 80) as u64, le32(h, 84) as u64);
            if at == 2 && esize == ENTRY_SIZE as u64 && n > 0 && n <= 1024 {
                (2 + (n * esize).div_ceil(sector)) * sector
            } else {
                2 * sector
            }
        }
        _ => 2 * sector,
    }
}

struct Slot([u8; ENTRY_SIZE]);

impl Slot {
    fn empty(&self) -> bool {
        self.0[0..16].iter().all(|b| *b == 0)
    }
    fn first(&self) -> u64 {
        le64(&self.0, 32)
    }
    fn last(&self) -> u64 {
        le64(&self.0, 40)
    }
    fn name(&self) -> String {
        String::from_utf16_lossy(&crate::table::utf16le(&self.0[56..128]))
    }
}

/// Builds the new table. `remove`: the slots (from 1) whose partitions go;
/// `disk_guid`: the GUID a disk without a table gets (unused otherwise).
pub fn build(src: &Source, remove: &[u32], new: &[NewPart], disk_guid: &str) -> Result<PcTable, String> {
    let s = src.sector;
    if !(512..=4096).contains(&s) || !s.is_power_of_two() {
        return Err(format!("sector size {s} is not one a GPT uses"));
    }
    let su = s as usize;
    let disk_sectors = src.disk_bytes / s;
    let head = src.head;
    if head.len() < 2 * su {
        return Err("fewer than two sectors of the disk were read".into());
    }
    let has_gpt = &head[su..su + 8] == b"EFI PART";
    let mbr_sig = head[510] == 0x55 && head[511] == 0xaa;
    let protective = mbr_sig && (0..4).any(|i| head[446 + i * 16 + 4] == 0xee);

    let (mbr, dguid, entries, first_usable, last_usable, mut slots) = if has_gpt {
        let h = &head[su..2 * su];
        if le32(h, 12) != 92 {
            return Err(format!("the disk's GPT header is {} bytes; this installer writes 92", le32(h, 12)));
        }
        let mut hc = h[0..92].to_vec();
        hc[16..20].fill(0);
        if crc32(&hc) != le32(h, 16) || le64(h, 24) != 1 {
            return Err("the disk's primary GPT header is damaged: nothing is written to it".into());
        }
        if le64(h, 32) + 1 != disk_sectors {
            return Err(format!(
                "the disk has {disk_sectors} sectors but its table puts the backup at {}: not this disk's table",
                le64(h, 32)
            ));
        }
        let (at, n, esize) = (le64(h, 72), le32(h, 80), le32(h, 84) as usize);
        if at != 2 || esize != ENTRY_SIZE || n == 0 || n > 1024 {
            return Err(format!("a GPT this installer doesn't write ({n} entries of {esize} bytes at {at})"));
        }
        let array = head
            .get(2 * su..2 * su + n as usize * ENTRY_SIZE)
            .ok_or("the disk's first sectors were read shorter than its entry array")?;
        if crc32(array) != le32(h, 88) {
            return Err("the disk's GPT entry array is damaged: nothing is written to it".into());
        }
        let slots: Vec<Slot> = array.chunks(ENTRY_SIZE).map(|c| Slot(c.try_into().unwrap())).collect();
        (head[..su].to_vec(), h[56..72].to_vec(), n, le64(h, 40), le64(h, 48), slots)
    } else if protective {
        return Err("the disk announces a GPT that can't be read: nothing is written to it".into());
    } else if mbr_sig && (0..4).any(|i| head[446 + i * 16 + 4] != 0) {
        return Err("the disk has an MBR partition table; the installer writes only to GPT disks or empty ones".into());
    } else {
        // No table: a protective MBR, a new header, 128 empty entries.
        let array_sectors = (NEW_ENTRIES as u64 * ENTRY_SIZE as u64).div_ceil(s);
        let mut m = vec![0u8; su];
        let e = &mut m[446..462];
        e[1..4].copy_from_slice(&[0x00, 0x02, 0x00]);
        e[4] = 0xee;
        e[5..8].copy_from_slice(&[0xff, 0xff, 0xff]);
        e[8..12].copy_from_slice(&1u32.to_le_bytes());
        e[12..16].copy_from_slice(&((disk_sectors - 1).min(u32::MAX as u64) as u32).to_le_bytes());
        m[510] = 0x55;
        m[511] = 0xaa;
        let g = guid_bytes(disk_guid)?;
        if g == [0; 16] {
            return Err("the new disk GUID is zero".into());
        }
        let first = 2 + array_sectors;
        let last = disk_sectors
            .checked_sub(2 + array_sectors)
            .filter(|l| *l > first)
            .ok_or("the disk is too small for a GPT")?;
        let slots = (0..NEW_ENTRIES).map(|_| Slot([0; ENTRY_SIZE])).collect();
        (m, g.to_vec(), NEW_ENTRIES, first, last, slots)
    };
    let array_sectors = (entries as u64 * ENTRY_SIZE as u64).div_ceil(s);
    let backup_header = disk_sectors - 1;
    let backup_array = backup_header - array_sectors;
    if last_usable >= backup_array || first_usable < 2 + array_sectors || first_usable > last_usable {
        return Err("the disk's table geometry doesn't hold together".into());
    }

    let mut changes = vec![];
    for &r in remove {
        let i = r.checked_sub(1).map(|i| i as usize).filter(|i| *i < slots.len());
        let Some(i) = i.filter(|i| !slots[*i].empty()) else {
            return Err(format!("the table has no partition {r} to remove"));
        };
        let (first, last, name) = (slots[i].first(), slots[i].last(), slots[i].name());
        slots[i].0 = [0; ENTRY_SIZE];
        changes.push(Change::Removed {
            slot: r,
            name: if name.is_empty() { format!("partition {r}") } else { name },
            first,
            last,
            size_bytes: (last - first + 1) * s,
        });
    }

    // The largest free region, aligned to MiB, between what stays.
    let align = (MIB / s).max(1);
    let mut used: Vec<(u64, u64)> = slots.iter().filter(|x| !x.empty()).map(|x| (x.first(), x.last())).collect();
    used.sort();
    let mut regions = vec![];
    let mut at = first_usable;
    for (f, l) in used.iter().copied().chain([(last_usable + 1, last_usable + 1)]) {
        let a = at.div_ceil(align) * align;
        let b = f.min(last_usable + 1) / align * align;
        if b > a {
            regions.push((a, b));
        }
        at = at.max(l + 1);
    }
    let Some(&(region_start, region_end)) = regions.iter().max_by(|x, y| (x.1 - x.0).cmp(&(y.1 - y.0)).then(y.0.cmp(&x.0))) else {
        return Err("the disk has no free space".into());
    };

    let mut placed = vec![];
    let mut next = region_start;
    for (k, n) in new.iter().enumerate() {
        let sectors = match n.size {
            NewSize::Bytes(b) => {
                if b == 0 || b % (align * s) != 0 {
                    return Err(format!("{}: {b} bytes is not a whole number of MiB", n.name));
                }
                b / s
            }
            NewSize::Rest { at_least } => {
                if k + 1 != new.len() {
                    return Err(format!("{}: only the last new partition takes the rest", n.name));
                }
                let left = region_end.saturating_sub(next);
                if left * s < at_least {
                    return Err(format!(
                        "{} would get {} bytes, less than its least of {at_least}: the free space is too small",
                        n.name,
                        left * s
                    ));
                }
                left
            }
        };
        let last = next + sectors - 1;
        if last >= region_end {
            return Err(format!(
                "the largest free region ({} bytes) doesn't hold {}: the free space is too small",
                (region_end - region_start) * s,
                n.name
            ));
        }
        let slot = slots
            .iter()
            .position(|x| x.empty())
            .ok_or("the table has no empty entry left for GlassineOS's partitions")?;
        let tg = guid_bytes(&n.type_guid)?;
        let ug = guid_bytes(&n.guid)?;
        if tg == [0; 16] || ug == [0; 16] {
            return Err(format!("{}: a zero GUID", n.name));
        }
        if slots.iter().any(|x| !x.empty() && x.0[16..32] == ug) {
            return Err(format!("{}: its unique GUID {} is already in the table", n.name, n.guid));
        }
        let units: Vec<u16> = n.name.encode_utf16().collect();
        if units.is_empty() || units.len() > 36 {
            return Err(format!("the name '{}' doesn't fit a GPT entry", n.name));
        }
        let mut e = [0u8; ENTRY_SIZE];
        e[0..16].copy_from_slice(&tg);
        e[16..32].copy_from_slice(&ug);
        e[32..40].copy_from_slice(&next.to_le_bytes());
        e[40..48].copy_from_slice(&last.to_le_bytes());
        for (i, u) in units.iter().enumerate() {
            e[56 + i * 2..58 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        slots[slot].0 = e;
        changes.push(Change::Added {
            slot: slot as u32 + 1,
            name: n.name.clone(),
            first: next,
            last,
            size_bytes: sectors * s,
            type_guid: n.type_guid.clone(),
            attributes: "0x0000000000000000".into(),
            guid: n.guid.clone(),
        });
        placed.push(Placed {
            slot: slot as u32 + 1,
            name: n.name.clone(),
            first: next,
            last,
            guid: n.guid.clone(),
        });
        next = last + 1;
    }

    // No two partitions overlap; all are inside the usable area.
    let mut all: Vec<(u64, u64)> = slots.iter().filter(|x| !x.empty()).map(|x| (x.first(), x.last())).collect();
    all.sort();
    for w in all.windows(2) {
        if w[1].0 <= w[0].1 {
            return Err(format!("partitions at sectors {} and {} would overlap", w[0].0, w[1].0));
        }
    }
    let mut array = vec![0u8; (array_sectors * s) as usize];
    for (i, x) in slots.iter().enumerate() {
        array[i * ENTRY_SIZE..(i + 1) * ENTRY_SIZE].copy_from_slice(&x.0);
    }
    let crc = crc32(&array[..entries as usize * ENTRY_SIZE]);
    let g = Header {
        sector: s,
        first_usable,
        last_usable,
        disk_guid: &dguid,
        entries,
        array_crc: crc,
    };
    let mut primary = mbr;
    primary.extend(header(&g, 1, backup_header, 2));
    primary.extend(&array);
    let mut backup = array.clone();
    backup.extend(header(&g, backup_header, 1, backup_array));
    Ok(PcTable {
        built: Built {
            sector: s,
            disk_sectors,
            primary,
            backup,
            backup_sector: backup_array,
            changes,
        },
        placed,
        disk_guid: guid(&dguid),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::table;

    const ESP: &str = "c12a7328-f81f-11d2-ba4b-00a0c93ec93b";
    const LINUX: &str = "0fc63daf-8483-4772-8e79-3d69d8477de4";

    fn new3() -> Vec<NewPart> {
        vec![
            NewPart {
                name: "glassine-boot".into(),
                size: NewSize::Bytes(64 * MIB),
                type_guid: ESP.into(),
                guid: "11111111-1111-4111-8111-111111111111".into(),
            },
            NewPart {
                name: "glassine-system".into(),
                size: NewSize::Bytes(128 * MIB),
                type_guid: LINUX.into(),
                guid: "22222222-2222-4222-8222-222222222222".into(),
            },
            NewPart {
                name: "glassine-userdata".into(),
                size: NewSize::Rest { at_least: 32 * MIB },
                type_guid: LINUX.into(),
                guid: "33333333-3333-4333-8333-333333333333".into(),
            },
        ]
    }

    fn image(t: &PcTable, size: u64) -> Vec<u8> {
        let mut d = vec![0u8; size as usize];
        d[..t.built.primary.len()].copy_from_slice(&t.built.primary);
        let o = t.built.backup_offset() as usize;
        d[o..o + t.built.backup.len()].copy_from_slice(&t.built.backup);
        d
    }

    #[test]
    fn a_blank_disk_gets_a_table_with_the_three_partitions() {
        let size = 512 * MIB;
        for sector in [512u64, 4096] {
            let blank = vec![0u8; 2 * sector as usize];
            let src = Source {
                head: &blank,
                disk_bytes: size,
                sector,
            };
            let t = build(&src, &[], &new3(), "44444444-4444-4444-8444-444444444444").unwrap();
            let d = image(&t, size);
            let r = table::read(&mut d.as_slice(), size, sector).unwrap();
            assert_eq!(r.kind, table::Kind::Gpt);
            assert!(r.primary_ok);
            let names: Vec<_> = r.entries.iter().map(|e| e.name.clone().unwrap()).collect();
            assert_eq!(names, ["glassine-boot", "glassine-system", "glassine-userdata"]);
            assert_eq!(r.entries[0].start, MIB);
            assert_eq!(r.entries[0].size, 64 * MIB);
            assert_eq!(r.entries[1].size, 128 * MIB);
            assert!(r.entries[2].size >= 300 * MIB);
            assert_eq!(r.entries[2].guid.as_deref(), Some("33333333-3333-4333-8333-333333333333"));
            // the backup copy alone reads the same
            let mut only_backup = d.clone();
            only_backup[sector as usize..2 * sector as usize].fill(0);
            let rb = table::read(&mut only_backup.as_slice(), size, sector).unwrap();
            assert!(!rb.primary_ok);
            assert_eq!(rb.entries, r.entries);
        }
    }

    #[test]
    fn kept_entries_and_the_mbr_stay_byte_for_byte_and_removed_space_is_reused() {
        let size = 1024 * MIB;
        // first a table with three partitions of "another system"
        let blank = vec![0u8; 1024];
        let other = vec![
            NewPart {
                name: "EFI system partition".into(),
                size: NewSize::Bytes(100 * MIB),
                type_guid: ESP.into(),
                guid: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
            },
            NewPart {
                name: "Basic data partition".into(),
                size: NewSize::Bytes(800 * MIB),
                type_guid: "ebd0a0a2-b9e5-4433-87c0-68b6b72699c7".into(),
                guid: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".into(),
            },
        ];
        let first = build(
            &Source {
                head: &blank,
                disk_bytes: size,
                sector: 512,
            },
            &[],
            &other,
            "55555555-5555-4555-8555-555555555555",
        )
        .unwrap();
        let head = first.built.primary.clone();
        // keep: the free space at the end (about 120 MiB) is too small
        let src = Source {
            head: &head,
            disk_bytes: size,
            sector: 512,
        };
        let e = build(&src, &[], &new3(), "x").unwrap_err();
        assert!(e.contains("too small"), "{e}");
        // replace the data partition: its space is reused
        let t = build(&src, &[2], &new3(), "x").unwrap();
        assert_eq!(t.built.primary[..512], head[..512]);
        assert_eq!(t.built.primary[1024 + 56..1024 + 72], head[1024 + 56..1024 + 72], "the disk GUID");
        assert_eq!(
            t.built.primary[1024 * 2..1024 * 2 + 128],
            head[1024 * 2..1024 * 2 + 128],
            "slot 1, byte for byte"
        );
        assert_eq!(t.placed[0].slot, 2);
        assert_eq!(t.placed[0].first, 101 * MIB / 512);
        assert!(matches!(t.built.changes[0], Change::Removed { slot: 2, .. }));
        let d = image(&t, size);
        let r = table::read(&mut d.as_slice(), size, 512).unwrap();
        assert_eq!(r.entries.len(), 4);
        // building again from the same source gives the same bytes
        let again = build(&src, &[2], &new3(), "x").unwrap();
        assert_eq!(again.built.primary, t.built.primary);
        assert_eq!(again.built.backup, t.built.backup);
    }

    #[test]
    fn damaged_mbr_and_unknown_tables_are_refused() {
        let mut mbr = vec![0u8; 1024];
        mbr[446 + 4] = 0x83;
        mbr[510] = 0x55;
        mbr[511] = 0xaa;
        let src = Source {
            head: &mbr,
            disk_bytes: 512 * MIB,
            sector: 512,
        };
        assert!(build(&src, &[], &new3(), "44444444-4444-4444-8444-444444444444").unwrap_err().contains("MBR"));
        mbr[446 + 4] = 0xee;
        let src = Source {
            head: &mbr,
            disk_bytes: 512 * MIB,
            sector: 512,
        };
        assert!(
            build(&src, &[], &new3(), "44444444-4444-4444-8444-444444444444")
                .unwrap_err()
                .contains("announces")
        );
        let blank = vec![0u8; 1024];
        let t = build(
            &Source {
                head: &blank,
                disk_bytes: 512 * MIB,
                sector: 512,
            },
            &[],
            &new3(),
            "44444444-4444-4444-8444-444444444444",
        )
        .unwrap();
        let mut bad = t.built.primary.clone();
        bad[1024 + 600] ^= 1; // inside the entry array
        let src = Source {
            head: &bad,
            disk_bytes: 512 * MIB,
            sector: 512,
        };
        assert!(build(&src, &[], &new3(), "x").unwrap_err().contains("damaged"));
        let src = Source {
            head: &t.built.primary,
            disk_bytes: 1024 * MIB,
            sector: 512,
        };
        assert!(build(&src, &[], &new3(), "x").unwrap_err().contains("not this disk"));
        assert_eq!(head_len(&t.built.primary, 512), 34 * 512);
        assert_eq!(head_len(&blank, 512), 1024);
    }
}
