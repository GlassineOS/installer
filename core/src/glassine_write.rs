// SPDX-License-Identifier: GPL-2.0-or-later
//! The computer's side of the RAM environment's protocol version 2: the
//! reads of version 1 (list, read, hash, reboot) and the three operations
//! that write (GlassineOS/images `docs/ramenv.md`, version 2):
//!
//! - ARM (5): the ranges that may be written from now on, as text lines
//!   `range DISK OFFSET LENGTH`; once per start of the environment (a second
//!   ARM, even after DISARM, is refused with status 8).
//! - WRITE (6): `length` bytes (at most [`WRITE_MAX`]) wholly inside one armed
//!   range, then their sha256; the device answers the sha256 of the range as
//!   it read it back from the disk after the write reached it.
//! - DISARM (7): every disk read-only again.
//!
//! The device has no notion of partitions: the computer builds every byte
//! and decides the order. Until ARM every disk is read-only, exactly as in
//! version 1 ([`crate::glassine_read`], which this module leaves as it is).
//!
//! The wire format is version 1's: a 32-byte request, a 64-byte response
//! header and exactly the payload it names, little-endian, offsets and
//! lengths in multiples of 512. A request carries version 2 and the device
//! answers in the request's version.

use crate::fastboot::{FbError, Transport};
use crate::glassine_read::{ALIGN, Disk, Listing, REQ_SIZE, RSP_SIZE, Reboot, TRANSFER, parse_listing};
use sha2::{Digest, Sha256};
use std::fmt;

pub use crate::glassine_read::{OP_HASH, OP_LIST, OP_READ, OP_REBOOT};

pub const VERSION: u16 = 2;
pub const OP_ARM: u16 = 5;
pub const OP_WRITE: u16 = 6;
pub const OP_DISARM: u16 = 7;

/// The most one WRITE carries.
pub const WRITE_MAX: u64 = 16 << 20;
/// The most ranges one ARM names, and the longest ARM text.
pub const ARM_RANGES_MAX: usize = 64;
pub const ARM_TEXT_MAX: usize = 16384;
/// The longest list this client accepts.
const LIST_MAX: u64 = 64 << 10;

/// What the device answers in a response's status (version 1's and 2's).
pub const STATUS_NAMES: &[(u16, &str)] = &[
    (0, "ok"),
    (1, "bad request"),
    (2, "unknown operation"),
    (3, "no such disk"),
    (4, "out of range"),
    (5, "the disk returned an error"),
    (6, "not armed"),
    (7, "bad data"),
    (8, "already armed"),
];

pub const STATUS_NOT_ARMED: u16 = 6;
pub const STATUS_BAD_DATA: u16 = 7;
pub const STATUS_ALREADY_ARMED: u16 = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RamError {
    Transport(FbError),
    /// The device refused or failed the request.
    Status {
        op: u16,
        status: u16,
        why: String,
    },
    /// A response that isn't the answer to the request just sent.
    OutOfStep(String),
    Malformed(String),
    /// Read bytes that don't match the sha256 the device sent with them.
    Corrupt {
        disk: u32,
        offset: u64,
    },
    /// The range read back after a WRITE isn't what was sent.
    ReadBackDiffers {
        disk: u32,
        offset: u64,
        length: u64,
    },
    /// Not sent: a request this client can't make (unaligned, too long, no list).
    Refused(String),
}

impl fmt::Display for RamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RamError::Transport(e) => write!(f, "{e}"),
            RamError::Status { op, status, why } => {
                let name = STATUS_NAMES.iter().find(|(s, _)| s == status).map_or("unknown status", |(_, n)| n);
                write!(f, "the RAM environment refused operation {op}: {name} ({why})")
            }
            RamError::OutOfStep(w) => write!(f, "the RAM environment's answers are out of step ({w})"),
            RamError::Malformed(w) => write!(f, "malformed answer from the RAM environment: {w}"),
            RamError::Corrupt { disk, offset } => write!(f, "disk {disk}: the data read at {offset} doesn't match its checksum"),
            RamError::ReadBackDiffers { disk, offset, length } => write!(
                f,
                "disk {disk}: the {length} bytes written at {offset} read back differently: the range may hold anything now"
            ),
            RamError::Refused(w) => write!(f, "not sent: {w}"),
        }
    }
}

impl std::error::Error for RamError {}

impl From<FbError> for RamError {
    fn from(e: FbError) -> Self {
        RamError::Transport(e)
    }
}

/// Whether writes are armed, from the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Writes {
    Off,
    Armed(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListingV2 {
    pub listing: Listing,
    pub writes: Writes,
}

/// Parses a version 2 list: `glassine-ramread 2`, version 1's lines, and
/// `writes off` or `writes armed N`.
pub fn parse_listing_v2(text: &str) -> Result<ListingV2, RamError> {
    let (first, rest) = text.split_once('\n').unwrap_or((text, ""));
    match first.strip_prefix("glassine-ramread ").map(str::parse::<u16>) {
        Some(Ok(VERSION)) => {}
        Some(Ok(v)) => return Err(RamError::Refused(format!("protocol version {v}; this client writes with version {VERSION}"))),
        _ => return Err(RamError::Malformed("list: no glassine-ramread line".into())),
    }
    let mut writes = None;
    for l in rest.lines() {
        match l.split(' ').collect::<Vec<_>>().as_slice() {
            ["writes", "off"] => writes = Some(Writes::Off),
            ["writes", "armed", n] => writes = Some(Writes::Armed(n.parse().map_err(|_| RamError::Malformed(format!("list: {l}")))?)),
            ["writes", ..] => return Err(RamError::Malformed(format!("list: {l}"))),
            _ => {}
        }
    }
    let writes = writes.ok_or_else(|| RamError::Malformed("list: no writes line".into()))?;
    // Version 1's lines are the same; its parser checks them (it ignores the writes line).
    let mut listing = parse_listing(&format!("glassine-ramread 1\n{rest}")).map_err(|e| RamError::Malformed(e.to_string()))?;
    listing.version = VERSION;
    Ok(ListingV2 { listing, writes })
}

/// A request's 32 bytes, version 2.
pub fn encode_request(op: u16, tag: u32, disk: u32, offset: u64, length: u64) -> [u8; REQ_SIZE] {
    let mut b = [0u8; REQ_SIZE];
    b[..4].copy_from_slice(b"GRRQ");
    b[4..6].copy_from_slice(&VERSION.to_le_bytes());
    b[6..8].copy_from_slice(&op.to_le_bytes());
    b[8..12].copy_from_slice(&tag.to_le_bytes());
    b[12..16].copy_from_slice(&disk.to_le_bytes());
    b[16..24].copy_from_slice(&offset.to_le_bytes());
    b[24..32].copy_from_slice(&length.to_le_bytes());
    b
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub op: u16,
    pub tag: u32,
    pub status: u16,
    pub length: u64,
    pub offset: u64,
    pub sha256: [u8; 32],
}

/// A version 2 response header.
pub fn parse_header(b: &[u8]) -> Result<Header, RamError> {
    if b.len() != RSP_SIZE || &b[..4] != b"GRRS" {
        return Err(RamError::OutOfStep("not a response header".into()));
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    if u16_at(4) != VERSION {
        return Err(RamError::OutOfStep(format!("a response of protocol version {}", u16_at(4))));
    }
    Ok(Header {
        op: u16_at(6),
        tag: u32::from_le_bytes(b[8..12].try_into().unwrap()),
        status: u16_at(12),
        length: u64_at(16),
        offset: u64_at(24),
        sha256: b[32..64].try_into().unwrap(),
    })
}

/// One range ARM names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Range {
    pub disk: u32,
    pub offset: u64,
    pub length: u64,
}

impl Range {
    pub fn end(&self) -> u64 {
        self.offset + self.length
    }
    pub fn contains(&self, disk: u32, offset: u64, length: u64) -> bool {
        disk == self.disk && offset >= self.offset && offset.checked_add(length).is_some_and(|e| e <= self.end())
    }
}

/// ARM's text: one `range DISK OFFSET LENGTH` line per range.
pub fn arm_text(ranges: &[Range]) -> String {
    ranges.iter().map(|r| format!("range {} {} {}\n", r.disk, r.offset, r.length)).collect()
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// A connection to a version 2 RAM environment.
pub struct Writer<T: Transport> {
    t: T,
    tag: u32,
    listing: Option<ListingV2>,
    armed: Vec<Range>,
}

impl<T: Transport> Writer<T> {
    pub fn new(t: T) -> Self {
        Writer {
            t,
            tag: 0,
            listing: None,
            armed: vec![],
        }
    }

    pub fn into_transport(self) -> T {
        self.t
    }

    /// The ranges this connection armed (empty when it found them armed).
    pub fn armed(&self) -> &[Range] {
        &self.armed
    }

    async fn recv_exact(&mut self, n: usize) -> Result<Vec<u8>, RamError> {
        let mut b = Vec::with_capacity(n);
        while b.len() < n {
            let want = (n - b.len()).min(TRANSFER);
            let got = self.t.recv_max(want).await?;
            if got.is_empty() {
                return Err(RamError::Transport(FbError::Usb("an empty transfer".into())));
            }
            if got.len() > want {
                return Err(RamError::OutOfStep(format!("{} bytes where {want} were left", got.len())));
            }
            b.extend_from_slice(&got);
        }
        Ok(b)
    }

    /// One request, then `out` (the announced payload, sent whole whatever
    /// the device will answer), then the header and at most `max` bytes.
    async fn exchange(&mut self, op: u16, disk: u32, offset: u64, length: u64, out: &[&[u8]], max: u64) -> Result<(Header, Vec<u8>), RamError> {
        self.tag = self.tag.wrapping_add(1);
        let tag = self.tag;
        self.t.send(&encode_request(op, tag, disk, offset, length)).await?;
        for part in out {
            for chunk in part.chunks(TRANSFER) {
                self.t.send(chunk).await?;
            }
        }
        let h = parse_header(&self.recv_exact(RSP_SIZE).await?)?;
        if h.op != op || h.tag != tag {
            return Err(RamError::OutOfStep(format!("answer to op {} tag {}, sent op {op} tag {tag}", h.op, h.tag)));
        }
        if h.status != 0 {
            if h.length > 1024 {
                return Err(RamError::Malformed(format!("a refusal with {} bytes of reason", h.length)));
            }
            let why = self.recv_exact(h.length as usize).await?;
            return Err(RamError::Status {
                op,
                status: h.status,
                why: String::from_utf8_lossy(&why).into_owned(),
            });
        }
        if h.length > max {
            return Err(RamError::Malformed(format!("{} bytes where at most {max} were asked", h.length)));
        }
        let data = self.recv_exact(h.length as usize).await?;
        Ok((h, data))
    }

    pub async fn list(&mut self) -> Result<ListingV2, RamError> {
        let (_, text) = self.exchange(OP_LIST, 0, 0, 0, &[], LIST_MAX).await?;
        let l = parse_listing_v2(std::str::from_utf8(&text).map_err(|_| RamError::Malformed("list: not UTF-8".into()))?)?;
        self.listing = Some(l.clone());
        Ok(l)
    }

    /// The disk `disk` as listed.
    pub fn disk(&self, disk: u32) -> Result<&Disk, RamError> {
        let l = self.listing.as_ref().ok_or_else(|| RamError::Refused("list the disks first".into()))?;
        l.listing.disks.get(disk as usize).ok_or_else(|| RamError::Refused(format!("no disk {disk}")))
    }

    fn range_ok(&self, disk: u32, offset: u64, length: u64, max: u64) -> Result<(), RamError> {
        let d = self.disk(disk)?;
        if length == 0
            || length > max
            || !offset.is_multiple_of(ALIGN)
            || !length.is_multiple_of(ALIGN)
            || !offset.is_multiple_of(d.sector as u64)
            || !length.is_multiple_of(d.sector as u64)
            || offset.checked_add(length).is_none_or(|e| e > d.size)
        {
            return Err(RamError::Refused(format!("{}: range {offset}+{length}", d.name)));
        }
        Ok(())
    }

    fn limits(&self) -> Result<(u64, u64), RamError> {
        let l = self.listing.as_ref().ok_or_else(|| RamError::Refused("list the disks first".into()))?;
        Ok((l.listing.max_read, l.listing.max_hash))
    }

    /// Bytes of a disk, checked against the sha256 the device sent with them.
    pub async fn read(&mut self, disk: u32, offset: u64, length: u64) -> Result<Vec<u8>, RamError> {
        let (max_read, _) = self.limits()?;
        self.range_ok(disk, offset, length, max_read)?;
        let (h, data) = self.exchange(OP_READ, disk, offset, length, &[], length).await?;
        if h.length != length || h.offset != offset {
            return Err(RamError::Malformed(format!("{} bytes at {} for {length} at {offset}", h.length, h.offset)));
        }
        if Sha256::digest(&data).as_slice() != h.sha256 {
            return Err(RamError::Corrupt { disk, offset });
        }
        Ok(data)
    }

    /// Reads any length (in reads of max-read).
    pub async fn read_all(&mut self, disk: u32, offset: u64, length: u64) -> Result<Vec<u8>, RamError> {
        let (max_read, _) = self.limits()?;
        let mut out = Vec::with_capacity(length as usize);
        let mut at = offset;
        while at < offset + length {
            let n = max_read.min(offset + length - at);
            out.extend(self.read(disk, at, n).await?);
            at += n;
        }
        Ok(out)
    }

    /// The device's sha256 of a range (at most max-hash).
    pub async fn hash(&mut self, disk: u32, offset: u64, length: u64) -> Result<[u8; 32], RamError> {
        let (_, max_hash) = self.limits()?;
        self.range_ok(disk, offset, length, max_hash)?;
        let (h, _) = self.exchange(OP_HASH, disk, offset, length, &[], 0).await?;
        if h.offset != offset {
            return Err(RamError::Malformed("a hash of another range".into()));
        }
        Ok(h.sha256)
    }

    /// Arms `ranges` for writing: once per start of the environment.
    pub async fn arm(&mut self, ranges: &[Range]) -> Result<u32, RamError> {
        if ranges.is_empty() || ranges.len() > ARM_RANGES_MAX {
            return Err(RamError::Refused(format!("{} ranges (1 to {ARM_RANGES_MAX})", ranges.len())));
        }
        for r in ranges {
            let d = self.disk(r.disk)?;
            if d.name.contains("boot") || d.name.contains("rpmb") {
                return Err(RamError::Refused(format!("{} is a boot or RPMB area, never written", d.name)));
            }
            self.range_ok(r.disk, r.offset, r.length, u64::MAX)?;
        }
        let mut sorted = ranges.to_vec();
        sorted.sort();
        if sorted.windows(2).any(|w| w[0].disk == w[1].disk && w[0].end() > w[1].offset) {
            return Err(RamError::Refused("armed ranges overlap".into()));
        }
        let text = arm_text(ranges);
        if text.len() > ARM_TEXT_MAX {
            return Err(RamError::Refused(format!("the ARM text is {} bytes", text.len())));
        }
        let (_, ans) = self.exchange(OP_ARM, 0, 0, text.len() as u64, &[text.as_bytes()], 64).await?;
        let ans = String::from_utf8_lossy(&ans).into_owned();
        let n: u32 = ans
            .strip_prefix("armed ")
            .and_then(|n| n.trim_end().parse().ok())
            .ok_or_else(|| RamError::Malformed(format!("ARM answered {ans:?}")))?;
        if n as usize != ranges.len() {
            return Err(RamError::Malformed(format!("ARM answered {n} ranges for {}", ranges.len())));
        }
        self.armed = ranges.to_vec();
        Ok(n)
    }

    /// Writes `data` at `offset` of `disk` and checks the device's read-back:
    /// the sha256 it answers must be the one of `data`. Returns it.
    pub async fn write(&mut self, disk: u32, offset: u64, data: &[u8]) -> Result<[u8; 32], RamError> {
        let len = data.len() as u64;
        self.range_ok(disk, offset, len, WRITE_MAX)?;
        if !self.armed.is_empty() && !self.armed.iter().any(|r| r.contains(disk, offset, len)) {
            return Err(RamError::Refused(format!("disk {disk} {offset}+{len} is in no range this connection armed")));
        }
        let sha: [u8; 32] = Sha256::digest(data).into();
        let (h, ans) = self.exchange(OP_WRITE, disk, offset, len, &[data, &sha], 32).await?;
        if ans.len() != 32 || h.offset != offset {
            return Err(RamError::Malformed(format!("WRITE answered {} bytes at {}", ans.len(), h.offset)));
        }
        if Sha256::digest(&ans).as_slice() != h.sha256 {
            return Err(RamError::Malformed("the WRITE answer doesn't match its checksum".into()));
        }
        let back: [u8; 32] = ans.try_into().unwrap();
        if back != sha {
            return Err(RamError::ReadBackDiffers { disk, offset, length: len });
        }
        Ok(back)
    }

    /// Every disk read-only again.
    pub async fn disarm(&mut self) -> Result<(), RamError> {
        self.exchange(OP_DISARM, 0, 0, 0, &[], 0).await?;
        self.armed.clear();
        Ok(())
    }

    /// Restarts the device (disarming first, on the device's side too).
    pub async fn reboot(&mut self, to: Reboot) -> Result<(), RamError> {
        let target = match to {
            Reboot::Normal => 0,
            Reboot::BootLoader => 1,
        };
        self.exchange(OP_REBOOT, target, 0, 0, &[], 0).await.map(|_| ())
    }
}

/// The sha256 of `data`, as hex.
pub fn sha_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing() {
        let t = "glassine-ramread 2\nmax-read 16777216\nmax-hash 67108864\nalign 512\nwrites armed 3\ndisk 0 mmcblk0 31268536320 512\n";
        let l = parse_listing_v2(t).unwrap();
        assert_eq!(l.writes, Writes::Armed(3));
        assert_eq!(l.listing.disks[0].name, "mmcblk0");
        assert_eq!(parse_listing_v2(&t.replace("armed 3", "off")).unwrap().writes, Writes::Off);
        assert!(parse_listing_v2(&t.replace("writes armed 3\n", "")).is_err());
        assert!(parse_listing_v2(&t.replace("ramread 2", "ramread 1")).is_err());
        assert!(parse_listing_v2(&t.replace("armed 3", "armed x")).is_err());
    }

    #[test]
    fn ranges() {
        let r = Range {
            disk: 0,
            offset: 1024,
            length: 1024,
        };
        assert!(r.contains(0, 1024, 1024) && r.contains(0, 1536, 512));
        assert!(!r.contains(0, 1536, 1024) && !r.contains(1, 1024, 512) && !r.contains(0, 512, 512));
        assert_eq!(arm_text(&[r]), "range 0 1024 1024\n");
    }
}
