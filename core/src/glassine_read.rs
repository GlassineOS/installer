// SPDX-License-Identifier: GPL-2.0-or-later
//! The computer's side of `glassine-read`: the protocol of the RAM environment
//! (GlassineOS/images `docs/ramenv.md`), which the boot loader starts from
//! memory to read a device's whole disks for the backup (spec 001 FR-009).
//!
//! The environment offers its disks read-only on one vendor-specific USB
//! interface ([`RR_CLASS`]/[`RR_SUBCLASS`]/[`RR_PROTOCOL`]) and answers four
//! operations: list, read a range, sha256 of a range, reboot. This module only
//! has those four; there is no operation that writes, here or on the device.
//!
//! The wire format is a byte stream: a request is 32 bytes, a response a
//! 64-byte header and exactly the payload length it names. The reader asks for
//! exactly what is left, so it never depends on short packets or zero-length
//! packets, and the same code runs over USB (natively and over WebUSB) and over
//! the test's simulated device. All numbers are little-endian.
//!
//! [`back_up`] reads a disk whole, checks every chunk against the sha256 the
//! device sent with it, and hands the bytes to a sink; its [`DiskRecord`]
//! keeps the sha256 of each hash range. [`verify_on_device`] asks the device for
//! the same ranges again (the device read them twice the same), and
//! [`check_copy`] reads the stored copy back from the computer and compares it
//! (the description's `read-back-compare`).

use crate::fastboot::{FbError, Transport};
use sha2::{Digest, Sha256};
use std::fmt;

/// The environment's interface: vendor class, 'G', 'R'.
pub const RR_CLASS: u8 = 0xff;
pub const RR_SUBCLASS: u8 = 0x47;
pub const RR_PROTOCOL: u8 = 0x52;

pub const VERSION: u16 = 1;
pub const REQ_SIZE: usize = 32;
pub const RSP_SIZE: usize = 64;
/// Offsets and lengths are multiples of this.
pub const ALIGN: u64 = 512;
/// The most the reader asks one transfer for.
pub const TRANSFER: usize = 1 << 20;
/// The longest list the reader accepts.
const LIST_MAX: u64 = 64 << 10;

pub const OP_LIST: u16 = 1;
pub const OP_READ: u16 = 2;
pub const OP_HASH: u16 = 3;
pub const OP_REBOOT: u16 = 4;

/// What the device answers in a response's status.
pub const STATUS_NAMES: &[(u16, &str)] = &[
    (0, "ok"),
    (1, "bad request"),
    (2, "unknown operation"),
    (3, "no such disk"),
    (4, "out of range"),
    (5, "the disk returned an error"),
];

/// Where a reboot goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reboot {
    /// The device starts normally: the previous system.
    Normal,
    /// The boot loader (fastboot).
    BootLoader,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    Transport(FbError),
    /// The device refused the request or failed it.
    Status {
        op: u16,
        status: u16,
        why: String,
    },
    /// A response that isn't the answer to the request just sent: the stream is
    /// out of step, and nothing after it can be trusted.
    OutOfStep(String),
    /// A response whose fields make no sense.
    Malformed(String),
    /// A chunk whose bytes don't match the sha256 the device sent with them.
    Corrupt {
        disk: String,
        offset: u64,
    },
    /// The device's hash of a range isn't the one read (it read differently twice).
    DeviceChanged {
        disk: String,
        offset: u64,
    },
    /// The stored copy isn't what was read.
    CopyDiffers {
        disk: String,
        offset: u64,
    },
    /// The sink (where the backup is stored) failed.
    Sink(String),
    /// Not what this reader can do (a range the device can't serve, a disk it
    /// didn't list).
    Refused(String),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Transport(e) => write!(f, "{e}"),
            ReadError::Status { op, status, why } => {
                let name = STATUS_NAMES.iter().find(|(s, _)| s == status).map_or("unknown status", |(_, n)| n);
                write!(f, "the device refused operation {op}: {name} ({why})")
            }
            ReadError::OutOfStep(w) => write!(f, "the device's answers are out of step ({w})"),
            ReadError::Malformed(w) => write!(f, "malformed answer: {w}"),
            ReadError::Corrupt { disk, offset } => write!(f, "{disk}: the data at {offset} doesn't match its checksum"),
            ReadError::DeviceChanged { disk, offset } => {
                write!(f, "{disk}: the device read the range at {offset} differently the second time")
            }
            ReadError::CopyDiffers { disk, offset } => write!(f, "{disk}: the stored copy differs in the range at {offset}"),
            ReadError::Sink(e) => write!(f, "storing the backup failed: {e}"),
            ReadError::Refused(w) => write!(f, "refused: {w}"),
        }
    }
}

impl std::error::Error for ReadError {}

impl From<FbError> for ReadError {
    fn from(e: FbError) -> Self {
        ReadError::Transport(e)
    }
}

/// One disk as the device lists it: its kernel name, never a serial or ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Disk {
    pub index: u32,
    pub name: String,
    pub size: u64,
    pub sector: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    pub version: u16,
    pub max_read: u64,
    pub max_hash: u64,
    pub align: u64,
    pub disks: Vec<Disk>,
}

/// Parses the list's text: `glassine-ramread 1`, `max-read N`, `max-hash N`,
/// `align N`, then `disk INDEX NAME SIZE SECTOR` lines with indexes 0, 1, ...
pub fn parse_listing(text: &str) -> Result<Listing, ReadError> {
    let bad = |w: &str| ReadError::Malformed(format!("list: {w}"));
    let mut lines = text.lines();
    let version = match lines.next().and_then(|l| l.strip_prefix("glassine-ramread ")) {
        Some(v) => v.parse::<u16>().map_err(|_| bad("version"))?,
        None => return Err(bad("no glassine-ramread line")),
    };
    if version != VERSION {
        return Err(ReadError::Refused(format!("protocol version {version}; this reader speaks {VERSION}")));
    }
    let (mut max_read, mut max_hash, mut align) = (None, None, None);
    let mut disks = Vec::new();
    for l in lines {
        let w: Vec<&str> = l.split(' ').collect();
        let n = |s: &str| s.parse::<u64>().map_err(|_| bad(l));
        match w.as_slice() {
            ["max-read", v] => max_read = Some(n(v)?),
            ["max-hash", v] => max_hash = Some(n(v)?),
            ["align", v] => align = Some(n(v)?),
            ["disk", i, name, size, sector] => {
                let index = i.parse::<u32>().map_err(|_| bad(l))?;
                if index as usize != disks.len() || name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b)) {
                    return Err(bad(l));
                }
                let sector = sector.parse::<u32>().map_err(|_| bad(l))?;
                disks.push(Disk {
                    index,
                    name: name.to_string(),
                    size: n(size)?,
                    sector,
                });
            }
            // a later version's lines: ignored
            _ => {}
        }
    }
    let (Some(max_read), Some(max_hash), Some(align)) = (max_read, max_hash, align) else {
        return Err(bad("missing max-read, max-hash or align"));
    };
    if align != ALIGN || max_read < ALIGN || !max_read.is_multiple_of(ALIGN) || max_hash < max_read || !max_hash.is_multiple_of(max_read) {
        return Err(bad("limits this reader can't use"));
    }
    if disks.iter().any(|d| !d.size.is_multiple_of(ALIGN)) {
        return Err(bad("a disk size that isn't a multiple of 512"));
    }
    Ok(Listing {
        version,
        max_read,
        max_hash,
        align,
        disks,
    })
}

/// The request's 32 bytes.
pub fn encode_request(op: u16, tag: u32, index: u32, offset: u64, length: u64) -> [u8; REQ_SIZE] {
    let mut b = [0u8; REQ_SIZE];
    b[..4].copy_from_slice(b"GRRQ");
    b[4..6].copy_from_slice(&VERSION.to_le_bytes());
    b[6..8].copy_from_slice(&op.to_le_bytes());
    b[8..12].copy_from_slice(&tag.to_le_bytes());
    b[12..16].copy_from_slice(&index.to_le_bytes());
    b[16..24].copy_from_slice(&offset.to_le_bytes());
    b[24..32].copy_from_slice(&length.to_le_bytes());
    b
}

/// A response header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub op: u16,
    pub tag: u32,
    pub status: u16,
    pub length: u64,
    pub offset: u64,
    pub sha256: [u8; 32],
}

pub fn parse_header(b: &[u8]) -> Result<Header, ReadError> {
    if b.len() != RSP_SIZE || &b[..4] != b"GRRS" {
        return Err(ReadError::OutOfStep("not a response header".into()));
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes(b[i..i + 4].try_into().unwrap());
    let u64_at = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    if u16_at(4) != VERSION {
        return Err(ReadError::OutOfStep(format!("response of protocol version {}", u16_at(4))));
    }
    Ok(Header {
        op: u16_at(6),
        tag: u32_at(8),
        status: u16_at(12),
        length: u64_at(16),
        offset: u64_at(24),
        sha256: b[32..64].try_into().unwrap(),
    })
}

/// A connection to the RAM environment. Only the four operations exist.
pub struct Reader<T: Transport> {
    t: T,
    tag: u32,
    listing: Option<Listing>,
}

impl<T: Transport> Reader<T> {
    pub fn new(t: T) -> Self {
        Reader { t, tag: 0, listing: None }
    }

    pub fn into_transport(self) -> T {
        self.t
    }

    /// Exactly `n` bytes, one transfer of at most [`TRANSFER`] at a time, each
    /// asking for no more than is left.
    async fn recv_exact(&mut self, n: usize) -> Result<Vec<u8>, ReadError> {
        let mut b = Vec::with_capacity(n);
        while b.len() < n {
            let want = (n - b.len()).min(TRANSFER);
            let got = self.t.recv_max(want).await?;
            if got.is_empty() {
                return Err(ReadError::Transport(FbError::Usb("an empty transfer".into())));
            }
            if got.len() > want {
                return Err(ReadError::OutOfStep(format!("{} bytes where {want} were left", got.len())));
            }
            b.extend_from_slice(&got);
        }
        Ok(b)
    }

    /// Sends one request and reads its header and payload (at most `max`
    /// bytes). A refusal is [`ReadError::Status`], with the device's reason.
    async fn exchange(&mut self, op: u16, index: u32, offset: u64, length: u64, max: u64) -> Result<(Header, Vec<u8>), ReadError> {
        self.tag = self.tag.wrapping_add(1);
        let tag = self.tag;
        self.t.send(&encode_request(op, tag, index, offset, length)).await?;
        let h = parse_header(&self.recv_exact(RSP_SIZE).await?)?;
        if h.op != op || h.tag != tag {
            return Err(ReadError::OutOfStep(format!("answer to op {} tag {}, sent op {op} tag {tag}", h.op, h.tag)));
        }
        if h.status != 0 {
            if h.length > 1024 {
                return Err(ReadError::Malformed(format!("a refusal with {} bytes of reason", h.length)));
            }
            let why = self.recv_exact(h.length as usize).await?;
            return Err(ReadError::Status {
                op,
                status: h.status,
                why: String::from_utf8_lossy(&why).into_owned(),
            });
        }
        if h.length > max {
            return Err(ReadError::Malformed(format!("{} bytes where at most {max} were asked", h.length)));
        }
        let data = self.recv_exact(h.length as usize).await?;
        Ok((h, data))
    }

    /// The disks and the limits. Kept for the reads that follow.
    pub async fn list(&mut self) -> Result<Listing, ReadError> {
        let (_, text) = self.exchange(OP_LIST, 0, 0, 0, LIST_MAX).await?;
        let l = parse_listing(std::str::from_utf8(&text).map_err(|_| ReadError::Malformed("list: not UTF-8".into()))?)?;
        self.listing = Some(l.clone());
        Ok(l)
    }

    fn range_ok(&self, disk: u32, offset: u64, length: u64, max: fn(&Listing) -> u64) -> Result<&Disk, ReadError> {
        let l = self.listing.as_ref().ok_or_else(|| ReadError::Refused("list the disks first".into()))?;
        let d = l.disks.get(disk as usize).ok_or_else(|| ReadError::Refused(format!("no disk {disk}")))?;
        if length == 0
            || length > max(l)
            || !offset.is_multiple_of(ALIGN)
            || !length.is_multiple_of(ALIGN)
            || offset.checked_add(length).is_none_or(|e| e > d.size)
        {
            return Err(ReadError::Refused(format!("{}: range {offset}+{length}", d.name)));
        }
        Ok(d)
    }

    /// Bytes `[offset, offset+length)` of a disk, checked against the sha256
    /// the device sent with them.
    pub async fn read(&mut self, disk: u32, offset: u64, length: u64) -> Result<Vec<u8>, ReadError> {
        let name = self.range_ok(disk, offset, length, |l| l.max_read)?.name.clone();
        let (h, data) = self.exchange(OP_READ, disk, offset, length, length).await?;
        if h.length != length || h.offset != offset {
            return Err(ReadError::Malformed(format!(
                "{name}: {} bytes at {} for {length} at {offset}",
                h.length, h.offset
            )));
        }
        if Sha256::digest(&data).as_slice() != h.sha256 {
            return Err(ReadError::Corrupt { disk: name, offset });
        }
        Ok(data)
    }

    /// The device's sha256 of `[offset, offset+length)`.
    pub async fn hash(&mut self, disk: u32, offset: u64, length: u64) -> Result<[u8; 32], ReadError> {
        self.range_ok(disk, offset, length, |l| l.max_hash)?;
        let (h, _) = self.exchange(OP_HASH, disk, offset, length, 0).await?;
        if h.offset != offset {
            return Err(ReadError::Malformed("a hash of another range".into()));
        }
        Ok(h.sha256)
    }

    /// Restarts the device; the environment answers first.
    pub async fn reboot(&mut self, to: Reboot) -> Result<(), ReadError> {
        let target = match to {
            Reboot::Normal => 0,
            Reboot::BootLoader => 1,
        };
        self.exchange(OP_REBOOT, target, 0, 0, 0).await.map(|_| ())
    }
}

/// What a backup of one disk read: its size, the sha256 of the whole, and the
/// sha256 of each range of `range` bytes (the last one shorter), for the
/// comparisons after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskRecord {
    pub disk: Disk,
    pub sha256: [u8; 32],
    pub range: u64,
    pub ranges: Vec<[u8; 32]>,
}

impl DiskRecord {
    pub fn sha256_hex(&self) -> String {
        self.sha256.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Reads disk `index` whole in reads of `max-read`, checking each, and gives
/// every byte to `sink` in order; `progress` gets the bytes done so far.
pub async fn back_up<T: Transport>(
    r: &mut Reader<T>,
    index: u32,
    mut sink: impl FnMut(&[u8]) -> Result<(), String>,
    mut progress: impl FnMut(u64, u64),
) -> Result<DiskRecord, ReadError> {
    let l = match &r.listing {
        Some(l) => l.clone(),
        None => r.list().await?,
    };
    let disk = l
        .disks
        .get(index as usize)
        .cloned()
        .ok_or_else(|| ReadError::Refused(format!("no disk {index}")))?;
    let (mut whole, mut part) = (Sha256::new(), Sha256::new());
    let mut ranges = Vec::new();
    let mut off = 0;
    while off < disk.size {
        let n = l.max_read.min(disk.size - off);
        let data = r.read(index, off, n).await?;
        sink(&data).map_err(ReadError::Sink)?;
        whole.update(&data);
        part.update(&data);
        off += n;
        if off.is_multiple_of(l.max_hash) || off == disk.size {
            ranges.push(part.finalize_reset().into());
        }
        progress(off, disk.size);
    }
    Ok(DiskRecord {
        disk,
        sha256: whole.finalize().into(),
        range: l.max_hash,
        ranges,
    })
}

/// Asks the device for the sha256 of each range again: equal means the device
/// read the same bytes twice.
pub async fn verify_on_device<T: Transport>(r: &mut Reader<T>, rec: &DiskRecord) -> Result<(), ReadError> {
    for (k, want) in rec.ranges.iter().enumerate() {
        let off = k as u64 * rec.range;
        let got = r.hash(rec.disk.index, off, rec.range.min(rec.disk.size - off)).await?;
        if &got != want {
            return Err(ReadError::DeviceChanged {
                disk: rec.disk.name.clone(),
                offset: off,
            });
        }
    }
    Ok(())
}

/// Reads the stored copy back (`fill` fills the buffer from it and returns how
/// many bytes it gave; 0 at its end) and compares it with what was read, range
/// by range.
pub fn check_copy(rec: &DiskRecord, mut fill: impl FnMut(&mut [u8]) -> Result<usize, String>) -> Result<(), ReadError> {
    let mut buf = vec![0u8; TRANSFER];
    let mut total = 0u64;
    for (k, want) in rec.ranges.iter().enumerate() {
        let off = k as u64 * rec.range;
        let len = rec.range.min(rec.disk.size - off);
        let mut h = Sha256::new();
        let mut done = 0;
        while done < len {
            let want_n = (len - done).min(buf.len() as u64) as usize;
            let n = fill(&mut buf[..want_n]).map_err(ReadError::Sink)?;
            if n == 0 {
                return Err(ReadError::CopyDiffers {
                    disk: rec.disk.name.clone(),
                    offset: off + done,
                });
            }
            h.update(&buf[..n]);
            done += n as u64;
        }
        total += len;
        if h.finalize().as_slice() != want {
            return Err(ReadError::CopyDiffers {
                disk: rec.disk.name.clone(),
                offset: off,
            });
        }
    }
    // nothing past the disk's end
    if fill(&mut buf[..1]).map_err(ReadError::Sink)? != 0 {
        return Err(ReadError::CopyDiffers {
            disk: rec.disk.name.clone(),
            offset: total,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing() {
        let t = "glassine-ramread 1\nmax-read 16777216\nmax-hash 67108864\nalign 512\ndisk 0 mmcblk1 31268536320 512\ndisk 1 mmcblk1boot0 4194304 512\n";
        let l = parse_listing(t).unwrap();
        assert_eq!(l.disks.len(), 2);
        assert_eq!(l.disks[1].name, "mmcblk1boot0");
        assert!(parse_listing("glassine-ramread 2\n").is_err());
        assert!(parse_listing(&t.replace("disk 1", "disk 3")).is_err());
        assert!(parse_listing(&t.replace("align 512", "align 4096")).is_err());
        assert!(parse_listing(&t.replace("mmcblk1boot0", "../x")).is_err());
        assert!(parse_listing(&t.replace("4194304", "4194305")).is_err());
        assert!(parse_listing(&t.replace("max-hash 67108864", "max-hash 1000")).is_err());
        assert!(parse_listing(&format!("{t}future-key 3\n")).is_ok());
    }

    #[test]
    fn header_roundtrip() {
        let r = encode_request(OP_READ, 7, 1, 512, 1024);
        assert_eq!(&r[..4], b"GRRQ");
        assert_eq!(u64::from_le_bytes(r[24..32].try_into().unwrap()), 1024);
        let mut h = [0u8; RSP_SIZE];
        h[..4].copy_from_slice(b"GRRS");
        h[4] = 1;
        assert_eq!(parse_header(&h).unwrap().status, 0);
        h[4] = 2;
        assert!(matches!(parse_header(&h), Err(ReadError::OutOfStep(_))));
        assert!(parse_header(&h[..63]).is_err());
    }
}
