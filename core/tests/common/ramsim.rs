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
