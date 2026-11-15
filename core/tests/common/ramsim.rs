// SPDX-License-Identifier: GPL-2.0-or-later
//! A simulated RAM environment: the device's side of the read-only disk
//! protocol (as GlassineOS/images `ramenv/glassine-ramread.c` speaks it) over
//! a transport that behaves like a USB bulk pipe. An IN transfer ends when its
//! buffer (rounded up to 512-byte packets) is full or a short packet arrives,
//! and one that asks for more than the device is going to send waits, which
//! here is a timeout.

use glassine_installer::fastboot::{FbError, Transport};
use glassine_installer::glassine_read::*;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;

pub const MPS: usize = 512;

/// What the simulated device does wrong, if anything.
#[derive(Default, Clone, Copy)]
pub struct Faults {
    /// flip one byte of the payload of the n-th read (its sha256 stays the true one)
    pub corrupt_read: Option<usize>,
    /// the hash answers come from data changed after the backup
    pub changed_after: bool,
    /// answer with the previous request's tag
    pub wrong_tag: bool,
    /// list disk 0 one MiB larger than it is
    pub lie_about_size: bool,
}

pub struct Device {
    pub disks: Vec<(String, Vec<u8>)>,
    pub max_read: u64,
    pub max_hash: u64,
    pub faults: Faults,
    /// bytes waiting to go IN, and the positions (from the front) where a
    /// device write ended with a short packet
    pub pending: VecDeque<u8>,
    pub short_at: VecDeque<usize>,
    pub ops: Vec<u16>,
    pub reads: usize,
    pub rebooted: Option<u32>,
}

impl Device {
    pub fn new(disks: Vec<(String, Vec<u8>)>, faults: Faults) -> Self {
        Device {
            disks,
            max_read: 4 << 20,
            max_hash: 8 << 20,
            faults,
            pending: VecDeque::new(),
            short_at: VecDeque::new(),
            ops: Vec::new(),
            reads: 0,
            rebooted: None,
        }
    }

    /// One device write: its bytes, sent as packets of 512, the last one short
    /// unless the length is a multiple of 512 (no zero-length packet).
    fn write(&mut self, b: &[u8]) {
        for chunk in b.chunks(1 << 20) {
            self.pending.extend(chunk);
            if chunk.len() % MPS != 0 {
                self.short_at.push_back(self.pending.len());
            }
        }
    }

    fn respond(&mut self, op: u16, tag: u32, status: u16, offset: u64, sha: [u8; 32], payload: &[u8]) {
        let mut h = [0u8; RSP_SIZE];
        h[..4].copy_from_slice(b"GRRS");
        h[4..6].copy_from_slice(&1u16.to_le_bytes());
        h[6..8].copy_from_slice(&op.to_le_bytes());
        h[8..12].copy_from_slice(&tag.to_le_bytes());
        h[12..14].copy_from_slice(&status.to_le_bytes());
        h[16..24].copy_from_slice(&(payload.len() as u64).to_le_bytes());
        h[24..32].copy_from_slice(&offset.to_le_bytes());
        h[32..64].copy_from_slice(&sha);
        self.write(&h);
        if !payload.is_empty() {
            self.write(payload);
        }
    }

    pub fn request(&mut self, b: &[u8]) {
        assert_eq!(b.len(), REQ_SIZE, "a request is one 32-byte transfer");
        assert_eq!(&b[..4], b"GRRQ");
        let op = u16::from_le_bytes([b[6], b[7]]);
        let mut tag = u32::from_le_bytes(b[8..12].try_into().unwrap());
        let index = u32::from_le_bytes(b[12..16].try_into().unwrap());
        let off = u64::from_le_bytes(b[16..24].try_into().unwrap());
        let len = u64::from_le_bytes(b[24..32].try_into().unwrap());
        self.ops.push(op);
        if self.faults.wrong_tag && self.ops.len() > 1 {
            tag -= 1;
        }
        let refuse = |d: &mut Device, status: u16, why: &str| d.respond(op, tag, status, off, [0; 32], why.as_bytes());
        match op {
            OP_LIST => {
                let mut t = format!("glassine-ramread 1\nmax-read {}\nmax-hash {}\nalign 512\n", self.max_read, self.max_hash);
                for (i, (name, data)) in self.disks.iter().enumerate() {
                    let extra = if i == 0 && self.faults.lie_about_size { 1 << 20 } else { 0 };
                    t += &format!("disk {i} {name} {} 512\n", data.len() + extra);
                }
                self.respond(op, tag, 0, 0, [0; 32], t.as_bytes());
            }
            OP_READ | OP_HASH => {
                let max = if op == OP_READ { self.max_read } else { self.max_hash };
                let Some((_, data)) = self.disks.get(index as usize) else {
                    return refuse(self, 3, "no such disk");
                };
                if len == 0 || len > max || !off.is_multiple_of(512) || !len.is_multiple_of(512) || off + len > data.len() as u64 {
                    return refuse(self, 4, "out of range");
                }
                let mut bytes = data[off as usize..(off + len) as usize].to_vec();
                if op == OP_HASH && self.faults.changed_after {
                    bytes[0] ^= 1;
                }
                let sha: [u8; 32] = Sha256::digest(&bytes).into();
                if op == OP_HASH {
                    return self.respond(op, tag, 0, off, sha, &[]);
                }
                if self.faults.corrupt_read == Some(self.reads) {
                    bytes[len as usize / 2] ^= 0x40;
                }
                self.reads += 1;
                self.respond(op, tag, 0, off, sha, &bytes);
            }
            OP_REBOOT => {
                self.rebooted = Some(index);
                self.respond(op, tag, 0, 0, [0; 32], &[]);
            }
            _ => refuse(self, 2, "unknown operation"),
        }
    }
}

impl Transport for Device {
    async fn send(&mut self, data: &[u8]) -> Result<(), FbError> {
        self.request(data);
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
        self.recv_max(256).await
    }

    async fn recv_max(&mut self, max: usize) -> Result<Vec<u8>, FbError> {
        let buf = max.div_ceil(MPS) * MPS;
        // the transfer ends at a full buffer or at the first short packet in it
        let end = match self.short_at.front() {
            Some(&s) if s <= buf => s,
            _ => buf,
        };
        if self.pending.len() < end {
            return Err(FbError::Timeout(format!(
                "a {buf}-byte transfer that the device never fills ({} pending)",
                self.pending.len()
            )));
        }
        let out: Vec<u8> = self.pending.drain(..end).collect();
        if self.short_at.front() == Some(&end) {
            self.short_at.pop_front();
        }
        for s in self.short_at.iter_mut() {
            *s -= end;
        }
        Ok(out)
    }
}

/// Deterministic bytes that differ everywhere.
pub fn image(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 33) as u8
        })
        .collect()
}

pub fn disks() -> Vec<(String, Vec<u8>)> {
    vec![
        // not a multiple of max-read or max-hash, so the last chunk and range are short
        ("mmcblk0".into(), image((19 << 20) + 3 * 512, 1)),
        ("mmcblk0boot0".into(), image(4 << 20, 2)),
        ("mmcblk0boot1".into(), image(512, 3)),
    ]
}

// ---- version 2: the writes ------------------------------------------------------

/// A disk held sparsely: chunks of [`CHUNK`] bytes, zeros where never written.
#[derive(Clone)]
pub struct Sparse {
    pub size: u64,
    chunks: std::collections::BTreeMap<u64, Vec<u8>>,
}

pub const CHUNK: u64 = 1 << 20;

impl Sparse {
    pub fn new(size: u64) -> Sparse {
        Sparse {
            size,
            chunks: Default::default(),
        }
    }

    pub fn read(&self, off: u64, len: u64) -> Vec<u8> {
        let mut out = vec![0u8; len as usize];
        let mut at = off;
        while at < off + len {
            let c = at / CHUNK;
            let in_c = at % CHUNK;
            let n = (CHUNK - in_c).min(off + len - at);
            if let Some(d) = self.chunks.get(&c) {
                let o = (at - off) as usize;
                out[o..o + n as usize].copy_from_slice(&d[in_c as usize..(in_c + n) as usize]);
            }
            at += n;
        }
        out
    }

    pub fn write(&mut self, off: u64, data: &[u8]) {
        assert!(off + data.len() as u64 <= self.size, "a write past the disk's end");
        let mut at = off;
        let end = off + data.len() as u64;
        while at < end {
            let c = at / CHUNK;
            let in_c = at % CHUNK;
            let n = (CHUNK - in_c).min(end - at);
            let d = self.chunks.entry(c).or_insert_with(|| vec![0u8; CHUNK as usize]);
            let o = (at - off) as usize;
            d[in_c as usize..(in_c + n) as usize].copy_from_slice(&data[o..o + n as usize]);
            at += n;
        }
    }
}

/// What the version 2 device does wrong, if anything.
#[derive(Default, Clone, Copy, Debug)]
pub struct WriteFaults {
    /// The n-th WRITE (from 0) writes half its bytes, then the device is gone.
    pub cut_write: Option<usize>,
    /// The n-th WRITE lands with one byte flipped (its read-back differs).
    pub flip_write: Option<usize>,
    /// The device is gone after this many requests (any kind).
    pub vanish_after_requests: Option<usize>,
}

/// The RAM environment of protocol version 2, as GlassineOS/images
/// `ramenv/glassine-ramread.c` speaks it (the addendum's pinned behaviour):
/// v1 requests answered as v1, ARM once per start, WRITE inside one armed
/// range, the payload always read whole before the answer, refusals with the
/// statuses of the contract. Disks are sparse.
pub struct RamEnv {
    pub disks: Vec<(String, Sparse)>,
    pub max_read: u64,
    pub max_hash: u64,
    pub faults: WriteFaults,
    pub armed: Option<Vec<(u32, u64, u64)>>,
    /// ARM was used up in this start (a refused ARM doesn't use it up).
    pub arm_used: bool,
    pub ops: Vec<u16>,
    pub writes: usize,
    /// (disk, offset, length) of every WRITE that reached a disk.
    pub written: Vec<(u32, u64, u64)>,
    pub rebooted: Option<u32>,
    pub gone: bool,
    inbuf: Vec<u8>,
    pending: VecDeque<u8>,
    short_at: VecDeque<usize>,
}

impl RamEnv {
    pub fn new(disks: Vec<(String, Sparse)>) -> RamEnv {
        RamEnv {
            disks,
            max_read: 4 << 20,
            max_hash: 64 << 20,
            faults: WriteFaults::default(),
            armed: None,
            arm_used: false,
            ops: vec![],
            writes: 0,
            written: vec![],
            rebooted: None,
            gone: false,
            inbuf: vec![],
            pending: VecDeque::new(),
            short_at: VecDeque::new(),
        }
    }

    /// A new start of the environment from the same disks.
    pub fn restart(&mut self) {
        self.armed = None;
        self.arm_used = false;
        self.rebooted = None;
        self.gone = false;
        self.inbuf.clear();
        self.pending.clear();
        self.short_at.clear();
    }

    fn out(&mut self, b: &[u8]) {
        for chunk in b.chunks(1 << 20) {
            self.pending.extend(chunk);
            if chunk.len() % MPS != 0 {
                self.short_at.push_back(self.pending.len());
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn respond(&mut self, version: u16, op: u16, tag: u32, status: u16, offset: u64, sha: [u8; 32], payload: &[u8]) {
        let mut h = [0u8; RSP_SIZE];
        h[..4].copy_from_slice(b"GRRS");
        h[4..6].copy_from_slice(&version.to_le_bytes());
        h[6..8].copy_from_slice(&op.to_le_bytes());
        h[8..12].copy_from_slice(&tag.to_le_bytes());
        h[12..14].copy_from_slice(&status.to_le_bytes());
        h[16..24].copy_from_slice(&(payload.len() as u64).to_le_bytes());
        h[24..32].copy_from_slice(&offset.to_le_bytes());
        h[32..64].copy_from_slice(&sha);
        self.out(&h);
        if !payload.is_empty() {
            self.out(payload);
        }
    }

    /// The bytes a request announces after itself.
    fn payload_len(version: u16, op: u16, len: u64) -> u64 {
        match (version, op) {
            (2, 5) => len,
            (2, 6) => len + 32,
            _ => 0,
        }
    }

    fn feed(&mut self, b: &[u8]) {
        self.inbuf.extend_from_slice(b);
        loop {
            if self.inbuf.len() < REQ_SIZE {
                return;
            }
            let r = &self.inbuf[..REQ_SIZE];
            assert_eq!(&r[..4], b"GRRQ", "out of step: not a request");
            let version = u16::from_le_bytes([r[4], r[5]]);
            let op = u16::from_le_bytes([r[6], r[7]]);
            let len = u64::from_le_bytes(r[24..32].try_into().unwrap());
            let need = REQ_SIZE as u64 + Self::payload_len(version, op, len);
            if (self.inbuf.len() as u64) < need {
                return;
            }
            let all: Vec<u8> = self.inbuf.drain(..need as usize).collect();
            self.handle(&all[..REQ_SIZE], &all[REQ_SIZE..]);
        }
    }

    fn handle(&mut self, r: &[u8], payload: &[u8]) {
        let version = u16::from_le_bytes([r[4], r[5]]);
        let op = u16::from_le_bytes([r[6], r[7]]);
        let tag = u32::from_le_bytes(r[8..12].try_into().unwrap());
        let index = u32::from_le_bytes(r[12..16].try_into().unwrap());
        let off = u64::from_le_bytes(r[16..24].try_into().unwrap());
        let len = u64::from_le_bytes(r[24..32].try_into().unwrap());
        self.ops.push(op);
        if self.faults.vanish_after_requests.is_some_and(|k| self.ops.len() > k) {
            self.gone = true;
            return;
        }
        if version != 1 && version != 2 {
            return self.respond(1, op, tag, 1, off, [0; 32], b"bad version");
        }
        let refuse = |d: &mut RamEnv, status: u16, why: &str| d.respond(version, op, tag, status, off, [0; 32], why.as_bytes());
        let aligned = |o: u64, l: u64| o.is_multiple_of(512) && l.is_multiple_of(512);
        match op {
            OP_LIST => {
                let mut t = format!(
                    "glassine-ramread {version}\nmax-read {}\nmax-hash {}\nalign 512\n",
                    self.max_read, self.max_hash
                );
                if version == 2 {
                    t += &match &self.armed {
                        Some(a) => format!("writes armed {}\n", a.len()),
                        None => "writes off\n".into(),
                    };
                }
                for (i, (name, d)) in self.disks.iter().enumerate() {
                    t += &format!("disk {i} {name} {} 512\n", d.size);
                }
                self.respond(version, op, tag, 0, 0, [0; 32], t.as_bytes());
            }
            OP_READ | OP_HASH => {
                let max = if op == OP_READ { self.max_read } else { self.max_hash };
                let Some((_, d)) = self.disks.get(index as usize) else {
                    return refuse(self, 3, "no such disk");
                };
                if len == 0 || len > max || !aligned(off, len) || off + len > d.size {
                    return refuse(self, 4, "out of range");
                }
                let bytes = d.read(off, len);
                let sha: [u8; 32] = Sha256::digest(&bytes).into();
                if op == OP_HASH {
                    return self.respond(version, op, tag, 0, off, sha, &[]);
                }
                self.respond(version, op, tag, 0, off, sha, &bytes);
            }
            5 if version == 2 => {
                if self.arm_used {
                    return refuse(self, 8, "already armed in this start");
                }
                let Ok(text) = std::str::from_utf8(payload) else {
                    return refuse(self, 1, "not text");
                };
                if payload.len() > 16384 {
                    return refuse(self, 1, "too long");
                }
                let mut ranges: Vec<(u32, u64, u64)> = vec![];
                for l in text.lines().filter(|l| !l.is_empty()) {
                    let w: Vec<&str> = l.split(' ').collect();
                    let [kw, disk, o, n] = w.as_slice() else {
                        return refuse(self, 1, "parse");
                    };
                    let (Ok(o), Ok(n)) = (o.parse::<u64>(), n.parse::<u64>()) else {
                        return refuse(self, 1, "parse");
                    };
                    if *kw != "range" {
                        return refuse(self, 1, "parse");
                    }
                    let di = match disk.parse::<u32>() {
                        Ok(i) if (i as usize) < self.disks.len() => i,
                        Ok(_) => return refuse(self, 3, "no such disk"),
                        Err(_) => match self.disks.iter().position(|(nm, _)| nm == disk) {
                            Some(i) => i as u32,
                            None => return refuse(self, 3, "no such disk"),
                        },
                    };
                    let (name, d) = &self.disks[di as usize];
                    if name.contains("boot") || n == 0 || !aligned(o, n) || o + n > d.size {
                        return refuse(self, 4, "boot area, unaligned, empty or past the end");
                    }
                    if ranges.iter().any(|&(x, a, b)| x == di && o < a + b && a < o + n) {
                        return refuse(self, 4, "overlap");
                    }
                    ranges.push((di, o, n));
                }
                if ranges.len() > 64 {
                    return refuse(self, 1, "too many");
                }
                self.arm_used = true;
                let ans = format!("armed {}", ranges.len());
                self.armed = Some(ranges);
                self.respond(version, op, tag, 0, 0, Sha256::digest(ans.as_bytes()).into(), ans.as_bytes());
            }
            6 if version == 2 => {
                let (data, sha) = payload.split_at(payload.len() - 32);
                let Some(armed) = self.armed.clone() else {
                    return refuse(self, 6, "not armed");
                };
                if (index as usize) >= self.disks.len() {
                    return refuse(self, 3, "no such disk");
                }
                if len == 0 || len > 16 << 20 || !aligned(off, len) || !armed.iter().any(|&(x, a, b)| x == index && off >= a && off + len <= a + b) {
                    return refuse(self, 4, "not inside one armed range");
                }
                if Sha256::digest(data).as_slice() != sha {
                    return refuse(self, 7, "bad data");
                }
                let n = self.writes;
                self.writes += 1;
                let d = &mut self.disks[index as usize].1;
                if self.faults.cut_write == Some(n) {
                    d.write(off, &data[..data.len() / 2]);
                    self.gone = true;
                    return;
                }
                let mut landed = data.to_vec();
                if self.faults.flip_write == Some(n) {
                    landed[0] ^= 0x10;
                }
                d.write(off, &landed);
                self.written.push((index, off, len));
                let back: [u8; 32] = Sha256::digest(d.read(off, len)).into();
                self.respond(version, op, tag, 0, off, Sha256::digest(back).into(), &back);
            }
            7 if version == 2 => {
                self.armed = None;
                self.respond(version, op, tag, 0, 0, [0; 32], &[]);
            }
            OP_REBOOT => {
                self.armed = None;
                self.rebooted = Some(index);
                self.respond(version, op, tag, 0, 0, [0; 32], &[]);
                self.gone = true;
            }
            _ => refuse(self, 2, "unknown operation"),
        }
    }
}

impl Transport for RamEnv {
    async fn send(&mut self, data: &[u8]) -> Result<(), FbError> {
        if self.gone {
            return Err(FbError::Usb("no such device".into()));
        }
        self.feed(data);
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
        self.recv_max(256).await
    }

    async fn recv_max(&mut self, max: usize) -> Result<Vec<u8>, FbError> {
        let buf = max.div_ceil(MPS) * MPS;
        let end = match self.short_at.front() {
            Some(&s) if s <= buf => s,
            _ => buf,
        };
        if self.pending.len() < end || end == 0 {
            if self.gone && self.pending.is_empty() {
                return Err(FbError::Usb("no such device".into()));
            }
            return Err(FbError::Timeout(format!("a {buf}-byte transfer that the device never fills ({} pending)", self.pending.len())));
        }
        let out: Vec<u8> = self.pending.drain(..end).collect();
        if self.short_at.front() == Some(&end) {
            self.short_at.pop_front();
        }
        for s in self.short_at.iter_mut() {
            *s -= end;
        }
        Ok(out)
    }
}
