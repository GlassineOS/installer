// SPDX-License-Identifier: GPL-2.0-or-later
//! A PC disk's backup image (backup format 2) and its restore.
//!
//! The image holds every byte of the disk except the free space of the file
//! systems it reads ([`crate::used`]): the partition table's two copies, the
//! space before the first partition, the space between and after the
//! partitions, every partition it can't read whole, and the used blocks of
//! the ones it can. Of those bytes, aligned blocks that read as zeros are
//! recorded as zero ranges and the rest is stored as data.
//!
//! The stored stream (`disk<N>.img`, compressed by the edge) is:
//!
//! - a header: `GLIMG\0`, the format (u16, 2), the disk's size (u64), its
//!   logical sector size (u32);
//! - records in disk order, each a tag byte, the offset and the length (u64
//!   each): `D` is followed by the bytes and their SHA-256, `Z` by nothing
//!   (the range reads as zeros);
//! - an end record `E` whose offset field holds the number of data bytes and
//!   whose length field the number of records before it.
//!
//! Integers are little-endian. A range no record names is free space of a
//! file system: a restore leaves it as it finds it. Format 1 (the previous
//! backup format) is the whole disk's bytes, uncompressed in the stream.
//!
//! A restore checks the whole stored copy against the manifest before its
//! first write, writes the primary table region (the disk's first MiB)
//! last, and for each range reads the disk first and writes only where it
//! differs, then flushes and reads it back: a cut restore is run again and
//! goes on where the disk still differs.

use crate::lock::sha256_hex;
use crate::ops::hex;
use crate::pc_execute::{BackupDisk, BackupManifest, DiskId, Machine, PcEdge};
use crate::table::{self, Kind, ReadAt};
use crate::used;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// How much is read, written and hashed at once (and the largest data record).
pub const CHUNK: usize = 16 << 20;
/// Blocks that read as zeros are recorded as zero ranges at this size.
pub const ZERO_BLOCK: usize = 64 << 10;
/// Free space smaller than this (after aligning to 4 KiB) is stored anyway.
pub const MIN_SKIP: u64 = 256 << 10;
/// Written last by a restore: the primary partition table's region.
pub const HEAD: u64 = 1 << 20;
const ALIGN: u64 = 4096;
const MAGIC: &[u8; 6] = b"GLIMG\0";
pub const IMAGE_FORMAT: u16 = 2;
const HEADER: usize = 6 + 2 + 8 + 4;
const REC: usize = 1 + 8 + 8;

/// How one partition is kept, for the manifest and the person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartKept {
    pub partition: u32,
    pub start: u64,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fs: Option<String>,
    /// `used blocks`, or `whole: <why>`.
    pub kept: String,
    /// The bytes of the partition the image holds (data or zeros).
    pub bytes: u64,
}

/// What a backup image holds, beside its file in the manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageSummary {
    pub image_format: u16,
    /// Bytes stored as data.
    pub data_bytes: u64,
    /// Bytes recorded as zeros.
    pub zero_bytes: u64,
    /// Free space of file systems, not kept.
    pub free_bytes: u64,
    pub records: u64,
    #[serde(default)]
    pub partitions: Vec<PartKept>,
    /// Why the whole disk is kept, when its table couldn't be read or made sense.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub whole: Option<String>,
}

/// The ranges of a disk the image keeps, and how each partition is kept.
pub struct Keep {
    pub ranges: Vec<(u64, u64)>,
    pub partitions: Vec<PartKept>,
    pub whole: Option<String>,
}

fn align_in(a: u64, b: u64) -> (u64, u64) {
    (a.div_ceil(ALIGN) * ALIGN, b / ALIGN * ALIGN)
}

/// The complement of sorted, merged `holes` within `[0, size)`.
fn complement(holes: &[(u64, u64)], size: u64) -> Vec<(u64, u64)> {
    let mut out = vec![];
    let mut at = 0;
    for &(a, b) in holes {
        if a > at {
            out.push((at, a));
        }
        at = at.max(b);
    }
    if at < size {
        out.push((at, size));
    }
    out
}

/// What of the disk the image keeps: everything but the free space of the
/// file systems on its partitions, read from their own metadata.
pub fn keep(r: &mut dyn ReadAt, size: u64, sector: u64) -> Keep {
    let whole = |why: String| Keep {
        ranges: vec![(0, size)],
        partitions: vec![],
        whole: Some(why),
    };
    let t = match table::read(r, size, sector) {
        Ok(t) => t,
        Err(e) => return whole(format!("the partition table couldn't be read ({e})")),
    };
    let mut parts: Vec<(u32, u64, u64)> = match t.kind {
        Kind::Unreadable => return whole("the partition table is damaged".into()),
        Kind::None => vec![(0, 0, size)],
        Kind::Gpt | Kind::Mbr => t.entries.iter().map(|e| (e.index, e.start, e.size)).collect(),
    };
    parts.sort_by_key(|p| p.1);
    for w in parts.windows(2) {
        if w[0].1 + w[0].2 > w[1].1 {
            return whole(format!("partitions {} and {} overlap", w[0].0, w[1].0));
        }
    }
    if parts.iter().any(|p| p.1.checked_add(p.2).is_none_or(|e| e > size)) {
        return whole("a partition reaches past the disk's end".into());
    }
    let mut holes = vec![];
    let mut kept = vec![];
    for &(index, start, psize) in &parts {
        let a = used::partition(r, start, psize);
        // A disk without a table and without a file system: no partition.
        if index == 0 && a.kind.is_none() {
            continue;
        }
        let mut free_here = 0;
        let how = match &a.used {
            Ok(u) => {
                for (fa, fb) in complement(u, psize) {
                    let (x, y) = align_in(start + fa, start + fb);
                    if y > x && y - x >= MIN_SKIP {
                        holes.push((x, y));
                        free_here += y - x;
                    }
                }
                "used blocks".to_string()
            }
            Err(why) => format!("whole: {why}"),
        };
        kept.push(PartKept {
            partition: index,
            start,
            size: psize,
            fs: a.kind.clone(),
            kept: how,
            bytes: psize - free_here,
        });
    }
    used::normalize(&mut holes);
    Keep {
        ranges: complement(&holes, size),
        partitions: kept,
        whole: None,
    }
}

fn rec(tag: u8, off: u64, len: u64) -> [u8; REC] {
    let mut b = [0u8; REC];
    b[0] = tag;
    b[1..9].copy_from_slice(&off.to_le_bytes());
    b[9..17].copy_from_slice(&len.to_le_bytes());
    b
}

fn is_zero(b: &[u8]) -> bool {
    let (words, rest) = b.as_chunks::<8>();
    words.iter().all(|c| u64::from_ne_bytes(*c) == 0) && rest.iter().all(|x| *x == 0)
}

/// Writes the image of `disk` into the stored file `file` and checks it:
/// the stored stream read back, each data range against a re-read of the
/// disk, each zero range re-read as zeros. Returns the manifest's entry.
pub fn back_up_disk<M: Machine, E: PcEdge>(m: &mut M, edge: &mut E, step: &str, disk: &DiskId, file: &str) -> Result<BackupDisk, String> {
    let k = {
        let mut rd = Reader { m: &mut *m, disk: disk.index };
        keep(&mut rd, disk.size, disk.sector)
    };
    let total: u64 = k.ranges.iter().map(|(a, b)| b - a).sum();
    edge.backup_begin(file)?;
    let mut h = Sha256::new();
    let put = |edge: &mut E, h: &mut Sha256, b: &[u8]| -> Result<(), String> {
        h.update(b);
        edge.backup_append(b)
    };
    let mut head = Vec::with_capacity(HEADER);
    head.extend_from_slice(MAGIC);
    head.extend_from_slice(&IMAGE_FORMAT.to_le_bytes());
    head.extend_from_slice(&disk.size.to_le_bytes());
    head.extend_from_slice(&(disk.sector as u32).to_le_bytes());
    put(edge, &mut h, &head)?;
    let (mut data_bytes, mut zero_bytes, mut records) = (0u64, 0u64, 0u64);
    let mut buf = vec![0u8; CHUNK];
    let mut done = 0u64;
    // a zero range not yet written: it may go on into the next chunk
    let mut zero: Option<(u64, u64)> = None;
    for &(a, b) in &k.ranges {
        let mut at = a;
        while at < b {
            let n = (b - at).min(CHUNK as u64) as usize;
            m.read(disk.index, at, &mut buf[..n])?;
            // runs of zero and data blocks
            let mut i = 0;
            while i < n {
                let z = is_zero(&buf[i..(i + ZERO_BLOCK).min(n)]);
                let mut j = (i + ZERO_BLOCK).min(n);
                while j < n && is_zero(&buf[j..(j + ZERO_BLOCK).min(n)]) == z {
                    j = (j + ZERO_BLOCK).min(n);
                }
                let off = at + i as u64;
                let len = (j - i) as u64;
                if z {
                    zero = match zero {
                        Some((s, l)) if s + l == off => Some((s, l + len)),
                        Some((s, l)) => {
                            put(edge, &mut h, &rec(b'Z', s, l))?;
                            records += 1;
                            Some((off, len))
                        }
                        None => Some((off, len)),
                    };
                    zero_bytes += len;
                } else {
                    if let Some((s, l)) = zero.take() {
                        put(edge, &mut h, &rec(b'Z', s, l))?;
                        records += 1;
                    }
                    put(edge, &mut h, &rec(b'D', off, len))?;
                    put(edge, &mut h, &buf[i..j])?;
                    put(edge, &mut h, &Sha256::digest(&buf[i..j]))?;
                    records += 1;
                    data_bytes += len;
                }
                i = j;
            }
            at += n as u64;
            done += n as u64;
            edge.progress(step, done, total);
        }
    }
    if let Some((s, l)) = zero.take() {
        put(edge, &mut h, &rec(b'Z', s, l))?;
        records += 1;
    }
    put(edge, &mut h, &rec(b'E', data_bytes, records))?;
    edge.backup_end()?;
    let sha = hex(&h.finalize());
    // The check: the stored stream, parsed, against a re-read of the disk.
    let mut p = Parser::new(disk.size, disk.sector);
    let mut rb = vec![0u8; CHUNK];
    let mut back = |r: Record| -> Result<(), String> {
        match r {
            Record::Data(off, bytes) => {
                m.read(disk.index, off, &mut rb[..bytes.len()])?;
                if rb[..bytes.len()] != bytes[..] {
                    return Err(format!("the stored range at {off} ({} bytes) and the disk's bytes there differ", bytes.len()));
                }
            }
            Record::Zero(off, len) => {
                let mut at = 0;
                while at < len {
                    let n = (len - at).min(CHUNK as u64) as usize;
                    m.read(disk.index, off + at, &mut rb[..n])?;
                    if !is_zero(&rb[..n]) {
                        return Err(format!("the disk at {} doesn't read as zeros as stored", off + at));
                    }
                    at += n as u64;
                }
            }
        }
        Ok(())
    };
    edge.backup_read(file, &mut |b| p.feed(b, &mut back))
        .and_then(|_| p.finish().map(|_| ()))
        .map_err(|e| format!("reading the backup of disk {} back: {e}: not verified", disk.index))?;
    let end = p.finish()?;
    if p.sha() != sha || end != (data_bytes, records) {
        return Err(format!(
            "the backup of disk {} read back as sha256 {}, written as {sha}: not verified",
            disk.index,
            p.sha()
        ));
    }
    Ok(BackupDisk {
        disk: disk.clone(),
        file: file.to_string(),
        sha256: sha,
        image: Some(ImageSummary {
            image_format: IMAGE_FORMAT,
            data_bytes,
            zero_bytes,
            free_bytes: disk.size - total,
            records,
            partitions: k.partitions,
            whole: k.whole,
        }),
    })
}

/// Reads a machine's disk as the parsers read.
struct Reader<'m, M: Machine> {
    m: &'m mut M,
    disk: u32,
}

impl<M: Machine> ReadAt for Reader<'_, M> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        self.m.read(self.disk, offset, buf)
    }
}

/// A record of the stored stream, as handed out while it is parsed.
pub enum Record<'a> {
    Data(u64, &'a [u8]),
    Zero(u64, u64),
}

/// Parses the stored stream from pieces of any size, checking every data
/// record's hash, the records' order and bounds, and the end record.
pub struct Parser {
    size: u64,
    sector: u64,
    buf: Vec<u8>,
    header: bool,
    /// the next offset a record may start at
    next: u64,
    data: u64,
    records: u64,
    end: Option<(u64, u64)>,
    h: Sha256,
    /// Format 1: the whole disk's bytes, handed out in chunks.
    v1: bool,
    at: u64,
}

impl Parser {
    pub fn new(size: u64, sector: u64) -> Parser {
        Parser {
            size,
            sector,
            buf: vec![],
            header: false,
            next: 0,
            data: 0,
            records: 0,
            end: None,
            h: Sha256::new(),
            v1: false,
            at: 0,
        }
    }

    /// A parser of a format-1 image: the disk's bytes as they are.
    pub fn v1(size: u64) -> Parser {
        Parser {
            v1: true,
            header: true,
            ..Parser::new(size, 512)
        }
    }

    pub fn sha(&self) -> String {
        hex(&self.h.clone().finalize())
    }

    pub fn feed(&mut self, b: &[u8], each: &mut dyn FnMut(Record) -> Result<(), String>) -> Result<(), String> {
        self.h.update(b);
        self.buf.extend_from_slice(b);
        if self.v1 {
            while self.buf.len() >= CHUNK || (!self.buf.is_empty() && self.at + self.buf.len() as u64 >= self.size) {
                let n = self.buf.len().min(CHUNK);
                if self.at + n as u64 > self.size {
                    return Err("the image is longer than its disk".into());
                }
                each(Record::Data(self.at, &self.buf[..n]))?;
                self.at += n as u64;
                self.buf.drain(..n);
            }
            return Ok(());
        }
        let mut used = 0;
        loop {
            let b = &self.buf[used..];
            if !self.header {
                if b.len() < HEADER {
                    break;
                }
                if &b[0..6] != MAGIC || u16::from_le_bytes([b[6], b[7]]) != IMAGE_FORMAT {
                    return Err("not a backup image of format 2".into());
                }
                let size = u64::from_le_bytes(b[8..16].try_into().unwrap());
                let sector = u32::from_le_bytes(b[16..20].try_into().unwrap()) as u64;
                if size != self.size || sector != self.sector {
                    return Err(format!(
                        "the image is of a disk of {size} bytes ({sector}-byte sectors), the manifest's of {} ({})",
                        self.size, self.sector
                    ));
                }
                self.header = true;
                used += HEADER;
                continue;
            }
            if self.end.is_some() {
                if !b.is_empty() {
                    return Err("bytes after the image's end".into());
                }
                break;
            }
            if b.len() < REC {
                break;
            }
            let (tag, off, len) = (
                b[0],
                u64::from_le_bytes(b[1..9].try_into().unwrap()),
                u64::from_le_bytes(b[9..17].try_into().unwrap()),
            );
            if tag == b'E' {
                self.end = Some((off, len));
                used += REC;
                continue;
            }
            if off < self.next || off.checked_add(len).is_none_or(|e| e > self.size) || len == 0 {
                return Err(format!("a record at {off} ({len} bytes) out of order or past the disk"));
            }
            match tag {
                b'Z' => {
                    used += REC;
                    each(Record::Zero(off, len))?;
                }
                b'D' => {
                    if len > CHUNK as u64 {
                        return Err(format!("a data record of {len} bytes, more than the format allows"));
                    }
                    let need = REC + len as usize + 32;
                    if b.len() < need {
                        break;
                    }
                    let d = &b[REC..REC + len as usize];
                    if Sha256::digest(d)[..] != b[REC + len as usize..need] {
                        return Err(format!("the stored range at {off} ({len} bytes) doesn't match its checksum"));
                    }
                    each(Record::Data(off, d))?;
                    self.data += len;
                    used += need;
                }
                t => return Err(format!("an unknown record ({t:#04x})")),
            }
            self.records += 1;
            self.next = off + len;
        }
        self.buf.drain(..used);
        Ok(())
    }

    /// The end record's counts (data bytes, records), once all was fed.
    pub fn finish(&self) -> Result<(u64, u64), String> {
        if self.v1 {
            return if self.at == self.size && self.buf.is_empty() {
                Ok((self.size, self.size.div_ceil(CHUNK as u64)))
            } else {
                Err(format!("the image holds {} of the disk's {} bytes", self.at, self.size))
            };
        }
        match self.end {
            Some(e) if self.buf.is_empty() && e == (self.data, self.records) => Ok(e),
            Some(e) => Err(format!("the image's end record ({e:?}) doesn't match what it holds")),
            None => Err("the image ends early (no end record)".into()),
        }
    }
}

// ---- restore -----------------------------------------------------------------------

/// What a restore did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Restored {
    pub written_bytes: u64,
    pub same_bytes: u64,
    pub variables_written: Vec<String>,
}

/// The manifest of the backup the edge holds, checked.
pub fn manifest<E: PcEdge>(edge: &mut E) -> Result<(BackupManifest, String), String> {
    let b = edge
        .backup_get(crate::pc_execute::MANIFEST)?
        .ok_or("the backup has no manifest.json: not a finished backup")?;
    let m: BackupManifest = serde_json::from_slice(&b).map_err(|e| format!("manifest.json: {e}"))?;
    if m.kind != "pc" {
        return Err(format!("a backup of a {}, not of a PC", m.kind));
    }
    if !(1..=crate::pc_execute::BACKUP_FORMAT).contains(&m.backup_format) {
        return Err(format!("backup format {}, which this installer doesn't read", m.backup_format));
    }
    Ok((m, sha256_hex(&b)))
}

fn parser_for(bd: &BackupDisk, format: i64) -> Parser {
    if format == 1 {
        Parser::v1(bd.disk.size)
    } else {
        Parser::new(bd.disk.size, bd.disk.sector)
    }
}

/// Checks the whole stored copy of one disk against its manifest entry.
pub fn verify_stored<E: PcEdge>(edge: &mut E, bd: &BackupDisk, format: i64) -> Result<(), String> {
    let mut p = parser_for(bd, format);
    edge.backup_read(&bd.file, &mut |b| p.feed(b, &mut |_| Ok(())))?;
    p.finish()?;
    if p.sha() != bd.sha256 {
        return Err(format!("{} reads as sha256 {}, the manifest says {}", bd.file, p.sha(), bd.sha256));
    }
    Ok(())
}

/// Restores one disk's image onto disk `target` (the same size and sector
/// size): checked whole first, then written range by range where the disk
/// differs, each read back; the first MiB last.
pub fn restore_disk<M: Machine, E: PcEdge>(m: &mut M, edge: &mut E, bd: &BackupDisk, format: i64, target: u32) -> Result<Restored, String> {
    let d = m
        .disks()?
        .into_iter()
        .find(|d| d.index == target)
        .ok_or_else(|| format!("disk {target} isn't there"))?;
    if d.size != bd.disk.size || d.sector != bd.disk.sector {
        return Err(format!(
            "disk {target} has {} bytes in {}-byte sectors; the backup is of a disk of {} bytes in {}-byte sectors: nothing is written",
            d.size, d.sector, bd.disk.size, bd.disk.sector
        ));
    }
    verify_stored(edge, bd, format).map_err(|e| format!("the backup isn't sound ({e}): nothing is written"))?;
    let mut out = Restored::default();
    let mut deferred: Vec<(u64, Option<Vec<u8>>, u64)> = vec![];
    let mut p = parser_for(bd, format);
    let mut cur = vec![0u8; CHUNK];
    let total = bd.disk.size;
    let step = format!("restore disk {target}");
    {
        let mut each = |r: Record| -> Result<(), String> {
            let (off, data, len) = match r {
                Record::Data(o, b) => (o, Some(b), b.len() as u64),
                Record::Zero(o, l) => (o, None, l),
            };
            // the part in the first MiB waits until the end
            if off < HEAD {
                let cut = (HEAD - off).min(len);
                deferred.push((off, data.map(|b| b[..cut as usize].to_vec()), cut));
                if cut == len {
                    return Ok(());
                }
                return write_range(m, target, off + cut, data.map(|b| &b[cut as usize..]), len - cut, &mut cur, &mut out);
            }
            write_range(m, target, off, data, len, &mut cur, &mut out)
        };
        edge.backup_read(&bd.file, &mut |b| p.feed(b, &mut each))?;
    }
    p.finish()?;
    if p.sha() != bd.sha256 {
        return Err(format!("{} changed while it was restored: run the restore again", bd.file));
    }
    for (off, data, len) in std::mem::take(&mut deferred) {
        write_range(m, target, off, data.as_deref(), len, &mut cur, &mut out)?;
    }
    edge.progress(&step, total, total);
    m.flush(target)?;
    m.table_written(target);
    Ok(out)
}

/// Writes one range where the disk differs: compared in chunks, each chunk
/// that differs written, flushed and read back.
fn write_range<M: Machine>(m: &mut M, disk: u32, off: u64, data: Option<&[u8]>, len: u64, cur: &mut [u8], out: &mut Restored) -> Result<(), String> {
    let zeros = vec![0u8; CHUNK.min(len as usize)];
    let mut at = 0u64;
    while at < len {
        let n = (len - at).min(CHUNK as u64) as usize;
        let want: &[u8] = match data {
            Some(b) => &b[at as usize..at as usize + n],
            None => &zeros[..n],
        };
        m.read(disk, off + at, &mut cur[..n])?;
        if cur[..n] == *want {
            out.same_bytes += n as u64;
        } else {
            m.write(disk, off + at, want).map_err(|e| format!("writing disk {disk} at {}: {e}", off + at))?;
            m.flush(disk)?;
            m.read(disk, off + at, &mut cur[..n])?;
            if cur[..n] != *want {
                return Err(format!(
                    "disk {disk} at {} read back otherwise than written: stopped; run the restore again",
                    off + at
                ));
            }
            out.written_bytes += n as u64;
        }
        at += n as u64;
    }
    Ok(())
}

/// Puts back the firmware's boot entries and boot order the backup holds
/// (`efivars.json`): each saved `Boot####` that differs and the boot order.
/// Entries added since stay, outside the boot order.
pub fn restore_variables<M: Machine, E: PcEdge>(m: &mut M, edge: &mut E, man: &BackupManifest, out: &mut Restored) -> Result<(), String> {
    let b = edge
        .backup_get(&man.variables.file)?
        .ok_or_else(|| format!("the backup has no {}", man.variables.file))?;
    if sha256_hex(&b) != man.variables.sha256 {
        return Err(format!("{} doesn't match the manifest", man.variables.file));
    }
    let vars: std::collections::BTreeMap<String, String> = serde_json::from_slice(&b).map_err(|e| e.to_string())?;
    let order = crate::efivar::global_name("BootOrder");
    let mut names: Vec<&String> = vars.keys().filter(|n| **n != order).collect();
    if vars.contains_key(&order) {
        names.push(&order);
    }
    for n in names {
        let want = unhex(&vars[n])?;
        if m.var(n)?.as_deref() != Some(want.as_slice()) {
            m.set_var(n, &want)?;
            if m.var(n)?.as_deref() != Some(want.as_slice()) {
                return Err(format!("{n} read back otherwise than written"));
            }
            out.variables_written.push(n.clone());
        }
    }
    Ok(())
}

fn unhex(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("odd hex".into());
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}
