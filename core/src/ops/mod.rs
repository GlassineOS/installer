// SPDX-License-Identifier: GPL-2.0-or-later
//! The executor's step kinds, one module each, and what they share: the
//! partition table as the RAM environment reads it, the rules a write is
//! checked against, the bytes the table builder needs.
//!
//! Every op writes only through a [`crate::fastboot::Session`] (checked by
//! [`WriteRules`]) or a [`crate::glassine_write::Writer`] (inside the ranges
//! the executor armed), and returns its evidence: what it wrote and the
//! hashes it read back.

pub mod boot_loader;
pub mod copy_partition;
pub mod partition_table;
pub mod set_boot_slot;
pub mod write_base;
pub mod write_image;

use crate::fastboot::Transport;
use crate::fastboot::{Framing, WriteRules};
use crate::format1::{Description, Step};
use crate::glassine_write::{RamError, Writer};
use crate::table::{crc32, le32, le64};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;

/// What an op found wrong. `may_have_changed`: a write may have reached
/// the device (the step's failure escape applies, not just a retry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpError {
    pub why: String,
    pub may_have_changed: bool,
}

impl OpError {
    pub fn before(why: impl Into<String>) -> OpError {
        OpError {
            why: why.into(),
            may_have_changed: false,
        }
    }
    pub fn after(why: impl Into<String>) -> OpError {
        OpError {
            why: why.into(),
            may_have_changed: true,
        }
    }
}

impl fmt::Display for OpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.why)
    }
}

/// A RAM environment error: after the first WRITE was sent, the range may
/// hold anything.
pub fn ram_err(e: RamError, writing: bool) -> OpError {
    OpError {
        why: e.to_string(),
        may_have_changed: writing,
    }
}

/// What an op shows: key → value, kept in the record.
pub type Evidence = BTreeMap<String, String>;

pub fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// A text parameter of a step.
pub fn param<'a>(st: &'a Step, key: &str) -> Result<&'a str, OpError> {
    st.params
        .get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| OpError::before(format!("step {} has no parameter {key}", st.id)))
}

/// The partitions a description never lets anything write, by name: its
/// `never-write` rules that select names.
///
/// TODO(write_rules.rs): the planner's `write_rules` derives the complete
/// rules from a description mode (never-use departures, the oem allow-list,
/// `system*` over fastboot); this is the minimal subset the executor needs
/// until the CLI passes those in.
pub fn never_write(d: &Description) -> Vec<String> {
    let mut out = Vec::new();
    for r in d.partitions.iter().filter(|r| r.class == "never-write") {
        out.extend(r.select.name.iter().cloned());
        out.extend(r.select.names.iter().flatten().cloned());
    }
    out
}

/// Minimal fastboot write rules for `mode` (see [`never_write`]'s TODO):
/// the framing of its `equal-length-transfers` departure, its never-write
/// names, and `reboot-recovery` when it has `oem-reboot-recovery`.
pub fn minimal_write_rules(d: &Description, mode: &str) -> WriteRules {
    let m = d.modes.iter().find(|m| m.id == mode);
    let framing = m
        .and_then(|m| m.departures.iter().find(|x| x.kind == "equal-length-transfers"))
        .and_then(|x| x.params.get("length").and_then(|v| v.as_integer()))
        .map(|len| Framing::EqualLength { len: len as usize })
        .unwrap_or(Framing::Plain);
    let oem = match m.is_some_and(|m| m.departures.iter().any(|x| x.kind == "oem-reboot-recovery")) {
        true => vec!["reboot-recovery".to_string()],
        false => vec![],
    };
    WriteRules {
        framing,
        never_write: never_write(d),
        oem,
    }
}

/// One partition of a GPT as the executor uses it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub slot: u32,
    pub name: String,
    pub first: u64,
    pub last: u64,
    pub attributes: u64,
}

/// A GPT's primary copy as read: its bytes and what they hold.
#[derive(Debug, Clone)]
pub struct Gpt {
    pub sector: u64,
    /// Sector 0 to the end of the entry array.
    pub primary: Vec<u8>,
    pub entries: u32,
    pub backup_header: u64,
    pub parts: Vec<Part>,
}

impl Gpt {
    pub fn part(&self, name: &str) -> Option<&Part> {
        self.parts.iter().find(|p| p.name == name)
    }
    /// The byte range of a partition.
    pub fn extent(&self, name: &str) -> Option<(u64, u64)> {
        self.part(name).map(|p| (p.first * self.sector, (p.last - p.first + 1) * self.sector))
    }
}

/// The length of a primary table (sector 0 to the end of its entry array)
/// from its first two sectors, checked.
pub fn primary_len(head: &[u8], sector: u64) -> Result<u64, String> {
    let s = sector as usize;
    let h = head.get(s..2 * s).ok_or("shorter than a GPT header")?;
    if &h[0..8] != b"EFI PART" {
        return Err("no GPT header at sector 1".into());
    }
    let (at, n, esize) = (le64(h, 72), le32(h, 80) as u64, le32(h, 84) as u64);
    if at != 2 || esize != 128 || n == 0 || n > 1024 {
        return Err(format!("a GPT this installer doesn't write ({n} entries of {esize} bytes at {at})"));
    }
    Ok((2 + (n * esize).div_ceil(sector)) * sector)
}

/// Parses a primary table whose CRCs hold.
pub fn parse_gpt(primary: &[u8], sector: u64) -> Result<Gpt, String> {
    let len = primary_len(primary, sector)?;
    if (primary.len() as u64) < len {
        return Err("the table is shorter than its entry array".into());
    }
    let s = sector as usize;
    let h = &primary[s..2 * s];
    let mut hc = h[0..92].to_vec();
    hc[16..20].fill(0);
    if crc32(&hc) != le32(h, 16) {
        return Err("the header CRC is wrong".into());
    }
    let n = le32(h, 80);
    let array = &primary[2 * s..2 * s + n as usize * 128];
    if crc32(array) != le32(h, 88) {
        return Err("the entry array CRC is wrong".into());
    }
    let mut parts = Vec::new();
    for i in 0..n as usize {
        let e = &array[i * 128..(i + 1) * 128];
        if e[0..16].iter().all(|b| *b == 0) {
            continue;
        }
        let name = String::from_utf16_lossy(&crate::table::utf16le(&e[56..128]));
        parts.push(Part {
            slot: i as u32 + 1,
            name,
            first: le64(e, 32),
            last: le64(e, 40),
            attributes: le64(e, 48),
        });
    }
    Ok(Gpt {
        sector,
        primary: primary[..len as usize].to_vec(),
        entries: n,
        backup_header: le64(h, 32),
        parts,
    })
}

/// Reads the live primary table through the RAM environment.
pub async fn read_gpt<T: Transport>(w: &mut Writer<T>, disk: u32, sector: u64) -> Result<Gpt, String> {
    let head = w.read(disk, 0, 2 * sector).await.map_err(|e| e.to_string())?;
    let len = primary_len(&head, sector)?;
    let all = w.read_all(disk, 0, len).await.map_err(|e| e.to_string())?;
    parse_gpt(&all, sector)
}

/// The primary table as the backup copy at the disk's end holds it, with
/// `mbr` as sector 0: for a primary torn by a cut write (the backup copy is
/// always written first and checked).
pub async fn gpt_from_backup<T: Transport>(w: &mut Writer<T>, disk: u32, sector: u64, disk_bytes: u64, mbr: &[u8]) -> Result<Gpt, String> {
    if mbr.len() as u64 != sector || mbr[510..512] != [0x55, 0xaa] {
        return Err("no protective MBR to put in front of the backup copy".into());
    }
    let last = disk_bytes / sector - 1;
    let h = w.read(disk, last * sector, sector).await.map_err(|e| e.to_string())?;
    if &h[0..8] != b"EFI PART" {
        return Err("no GPT header in the disk's last sector".into());
    }
    let hsize = le32(&h, 12) as usize;
    if !(92..=sector as usize).contains(&hsize) {
        return Err(format!("a backup header of {hsize} bytes"));
    }
    let mut hc = h[..hsize].to_vec();
    hc[16..20].fill(0);
    if crc32(&hc) != le32(&h, 16) || le64(&h, 24) != last {
        return Err("the backup header is broken".into());
    }
    let (at, n, esize) = (le64(&h, 72), le32(&h, 80) as u64, le32(&h, 84) as u64);
    if esize != 128 || n == 0 || n > 1024 || at >= last {
        return Err(format!("a backup table this installer doesn't write ({n} entries of {esize} bytes at {at})"));
    }
    let array_sectors = (n * 128).div_ceil(sector);
    let array = w.read_all(disk, at * sector, array_sectors * sector).await.map_err(|e| e.to_string())?;
    if crc32(&array[..(n * 128) as usize]) != le32(&h, 88) {
        return Err("the backup entry array CRC is wrong".into());
    }
    let mut ph = h.clone();
    ph[24..32].copy_from_slice(&1u64.to_le_bytes());
    ph[32..40].copy_from_slice(&last.to_le_bytes());
    ph[72..80].copy_from_slice(&2u64.to_le_bytes());
    ph[16..20].fill(0);
    let c = crc32(&ph[..hsize]);
    ph[16..20].copy_from_slice(&c.to_le_bytes());
    let mut primary = mbr.to_vec();
    primary.extend_from_slice(&ph);
    primary.extend_from_slice(&array);
    parse_gpt(&primary, sector)
}

/// A unique partition GUID derived from the unit's own disk GUID and the
/// partition's name: the same on every build for this unit (a resumed
/// table step writes the same bytes again), different between units. The
/// core has no randomness; the text form is a version 8 UUID.
pub fn derived_guid(disk_guid: &[u8], name: &str) -> String {
    let mut h = Sha256::new();
    h.update(b"glassine-installer partition guid\0");
    h.update(disk_guid);
    h.update(name.as_bytes());
    let mut x: Vec<char> = hex(&h.finalize()[..16]).chars().collect();
    x[12] = '8';
    x[16] = ['8', '9', 'a', 'b'][x[16].to_digit(16).unwrap() as usize & 3];
    let s: String = x.into_iter().collect();
    format!("{}-{}-{}-{}-{}", &s[0..8], &s[8..12], &s[12..16], &s[16..20], &s[20..32])
}

/// A size as descriptions write it: `3GiB`, `64MiB`, `512KiB` or bytes.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    for (suf, m) in [("GiB", 1u64 << 30), ("MiB", 1 << 20), ("KiB", 1 << 10)] {
        if let Some(n) = s.strip_suffix(suf) {
            return n.trim().parse::<u64>().ok().and_then(|n| n.checked_mul(m));
        }
    }
    s.parse().ok()
}

/// Feeds written bytes into hashes of fixed ranges (from the start of the
/// partition), for the device-side check after a write: one sha256 per
/// `range` bytes, the last one shorter.
pub struct RangeHashes {
    pub range: u64,
    cur: Sha256,
    in_cur: u64,
    pub done: Vec<[u8; 32]>,
}

impl RangeHashes {
    pub fn new(range: u64) -> Self {
        RangeHashes {
            range,
            cur: Sha256::new(),
            in_cur: 0,
            done: vec![],
        }
    }
    pub fn feed(&mut self, mut b: &[u8]) {
        while !b.is_empty() {
            let n = ((self.range - self.in_cur) as usize).min(b.len());
            self.cur.update(&b[..n]);
            self.in_cur += n as u64;
            b = &b[n..];
            if self.in_cur == self.range {
                self.done.push(std::mem::take(&mut self.cur).finalize().into());
                self.in_cur = 0;
            }
        }
    }
    pub fn finish(mut self) -> Vec<[u8; 32]> {
        if self.in_cur > 0 {
            self.done.push(self.cur.finalize().into());
        }
        self.done
    }
}

/// Asks the device for each range's hash and compares: `start` is where the
/// ranges begin, `total` their length together.
pub async fn check_ranges<T: Transport>(w: &mut Writer<T>, disk: u32, start: u64, total: u64, range: u64, want: &[[u8; 32]]) -> Result<(), OpError> {
    for (k, h) in want.iter().enumerate() {
        let off = k as u64 * range;
        let len = range.min(total - off);
        let got = w.hash(disk, start + off, len).await.map_err(|e| ram_err(e, true))?;
        if &got != h {
            return Err(OpError::after(format!(
                "the device's hash of {len} bytes at {} isn't the one written",
                start + off
            )));
        }
    }
    Ok(())
}

/// The sha256 of `b`, as hex.
pub fn hex_sha(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}
