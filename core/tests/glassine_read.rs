// SPDX-License-Identifier: GPL-2.0-or-later
//! The `glassine-read` reader against a simulated RAM environment: the
//! device's side of the protocol (as GlassineOS/images `ramenv/glassine-ramread.c`
//! speaks it) over a transport that behaves like a USB bulk pipe. An IN
//! transfer ends when its buffer (rounded up to 512-byte packets) is full or a
//! short packet arrives, and one that asks for more than the device is going
//! to send waits, which here is a timeout. So a reader that asks for more than
//! is left fails these tests, as it would hang on a cable.

use glassine_installer::fastboot::{FbError, Transport};
use glassine_installer::glassine_read::*;
use sha2::{Digest, Sha256};
use std::collections::VecDeque;

const MPS: usize = 512;

/// What the simulated device does wrong, if anything.
#[derive(Default, Clone, Copy)]
struct Faults {
    /// flip one byte of the payload of the n-th read (its sha256 stays the true one)
    corrupt_read: Option<usize>,
    /// the hash answers come from data changed after the backup
    changed_after: bool,
    /// answer with the previous request's tag
    wrong_tag: bool,
    /// list disk 0 one MiB larger than it is
    lie_about_size: bool,
}

struct Device {
    disks: Vec<(String, Vec<u8>)>,
    max_read: u64,
    max_hash: u64,
    faults: Faults,
    /// bytes waiting to go IN, and the positions (from the front) where a
    /// device write ended with a short packet
    pending: VecDeque<u8>,
    short_at: VecDeque<usize>,
    ops: Vec<u16>,
    reads: usize,
    rebooted: Option<u32>,
}

impl Device {
    fn new(disks: Vec<(String, Vec<u8>)>, faults: Faults) -> Self {
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

    fn request(&mut self, b: &[u8]) {
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
fn image(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed;
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (x >> 33) as u8
        })
        .collect()
}

fn disks() -> Vec<(String, Vec<u8>)> {
    vec![
        // not a multiple of max-read or max-hash, so the last chunk and range are short
        ("mmcblk0".into(), image((19 << 20) + 3 * 512, 1)),
        ("mmcblk0boot0".into(), image(4 << 20, 2)),
        ("mmcblk0boot1".into(), image(512, 3)),
    ]
}

fn run<F: Future>(f: F) -> F::Output {
    pollster::block_on(f)
}

#[test]
fn backs_up_every_disk_and_verifies_it() {
    let want = disks();
    let mut r = Reader::new(Device::new(want.clone(), Faults::default()));
    let l = run(r.list()).unwrap();
    assert_eq!(
        l.disks.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(),
        ["mmcblk0", "mmcblk0boot0", "mmcblk0boot1"]
    );
    for d in &l.disks {
        let mut copy = Vec::new();
        let mut last = 0;
        let rec = run(back_up(
            &mut r,
            d.index,
            |b| {
                copy.extend_from_slice(b);
                Ok(())
            },
            |done, total| {
                assert!(done > last && done <= total);
                last = done;
            },
        ))
        .unwrap();
        let img = &want[d.index as usize].1;
        assert_eq!(copy.len(), img.len());
        assert!(copy == *img, "{}: the stored bytes are the disk's", d.name);
        assert_eq!(rec.sha256, <[u8; 32]>::from(Sha256::digest(img)), "{}", d.name);
        assert_eq!(rec.ranges.len(), img.len().div_ceil(8 << 20));
        run(verify_on_device(&mut r, &rec)).unwrap();
        let mut at = 0;
        check_copy(&rec, |b| {
            let n = b.len().min(copy.len() - at);
            b[..n].copy_from_slice(&copy[at..at + n]);
            at += n;
            Ok(n)
        })
        .unwrap();
    }
    let dev = r.into_transport();
    assert!(
        dev.ops.iter().all(|op| [OP_LIST, OP_READ, OP_HASH].contains(op)),
        "only list, read and hash were sent"
    );
    assert!(dev.pending.is_empty(), "every byte the device sent was read");
}

#[test]
fn a_changed_copy_is_caught() {
    let want = disks();
    let mut r = Reader::new(Device::new(want.clone(), Faults::default()));
    let rec = run(back_up(&mut r, 0, |_| Ok(()), |_, _| {})).unwrap();
    let img = &want[0].1;
    let check = |copy: Vec<u8>| {
        let mut at = 0;
        check_copy(&rec, move |b| {
            let n = b.len().min(copy.len() - at);
            b[..n].copy_from_slice(&copy[at..at + n]);
            at += n;
            Ok(n)
        })
    };
    let mut flipped = img.clone();
    flipped[(9 << 20) + 5] ^= 1;
    assert_eq!(
        check(flipped),
        Err(ReadError::CopyDiffers {
            disk: "mmcblk0".into(),
            offset: 8 << 20
        })
    );
    assert!(
        matches!(check(img[..img.len() - 512].to_vec()), Err(ReadError::CopyDiffers { .. })),
        "a short copy"
    );
    let mut longer = img.clone();
    longer.push(0);
    assert!(matches!(check(longer), Err(ReadError::CopyDiffers { .. })), "a copy with bytes past the end");
    assert!(check(img.clone()).is_ok());
}

#[test]
fn a_corrupt_chunk_is_caught() {
    let mut r = Reader::new(Device::new(
        disks(),
        Faults {
            corrupt_read: Some(2),
            ..Default::default()
        },
    ));
    let e = run(back_up(&mut r, 0, |_| Ok(()), |_, _| {})).unwrap_err();
    assert_eq!(
        e,
        ReadError::Corrupt {
            disk: "mmcblk0".into(),
            offset: 8 << 20
        }
    );
}

#[test]
fn a_device_that_reads_differently_twice_is_caught() {
    let mut r = Reader::new(Device::new(
        disks(),
        Faults {
            changed_after: true,
            ..Default::default()
        },
    ));
    let rec = run(back_up(&mut r, 1, |_| Ok(()), |_, _| {})).unwrap();
    assert_eq!(
        run(verify_on_device(&mut r, &rec)),
        Err(ReadError::DeviceChanged {
            disk: "mmcblk0boot0".into(),
            offset: 0
        })
    );
}

#[test]
fn answers_out_of_step_are_refused() {
    let mut r = Reader::new(Device::new(
        disks(),
        Faults {
            wrong_tag: true,
            ..Default::default()
        },
    ));
    run(r.list()).unwrap();
    assert!(matches!(run(r.read(0, 0, 512)), Err(ReadError::OutOfStep(_))));
}

#[test]
fn refusals_carry_the_reason() {
    let mut r = Reader::new(Device::new(
        disks(),
        Faults {
            lie_about_size: true,
            ..Default::default()
        },
    ));
    let l = run(r.list()).unwrap();
    let end = l.disks[0].size - 512;
    match run(r.read(0, end, 512)) {
        Err(ReadError::Status { op: OP_READ, status: 4, why }) => assert_eq!(why, "out of range"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn ranges_the_device_cant_serve_are_never_sent() {
    let mut r = Reader::new(Device::new(disks(), Faults::default()));
    assert!(matches!(run(r.read(0, 0, 512)), Err(ReadError::Refused(_))), "before the list");
    run(r.list()).unwrap();
    for (disk, off, len) in [
        (3, 0, 512),
        (0, 100, 512),
        (0, 0, 100),
        (0, 0, 0),
        (0, 0, (4 << 20) + 512),
        (2, 512, 512),
        (0, u64::MAX - 511, 512),
    ] {
        assert!(matches!(run(r.read(disk, off, len)), Err(ReadError::Refused(_))), "{disk} {off} {len}");
    }
    assert!(matches!(run(r.hash(0, 0, (8 << 20) + 512)), Err(ReadError::Refused(_))));
    assert_eq!(r.into_transport().ops, [OP_LIST], "nothing but the list went out");
}

#[test]
fn reboot_goes_where_asked() {
    let mut r = Reader::new(Device::new(disks(), Faults::default()));
    run(r.reboot(Reboot::BootLoader)).unwrap();
    assert_eq!(r.into_transport().rebooted, Some(1));
    let mut r = Reader::new(Device::new(disks(), Faults::default()));
    run(r.reboot(Reboot::Normal)).unwrap();
    assert_eq!(r.into_transport().rebooted, Some(0));
}

#[test]
fn a_sink_failure_stops_the_backup() {
    let mut r = Reader::new(Device::new(disks(), Faults::default()));
    let mut n = 0;
    let e = run(back_up(
        &mut r,
        0,
        |_| {
            n += 1;
            if n == 2 { Err("disk full".into()) } else { Ok(()) }
        },
        |_, _| {},
    ))
    .unwrap_err();
    assert_eq!(e, ReadError::Sink("disk full".into()));
}

/// The simulation itself: a reader that asked for more than is left would wait
/// on a real pipe; here that is a timeout.
#[test]
fn over_asking_times_out_in_the_simulation() {
    let mut d = Device::new(disks(), Faults::default());
    run(d.send(&encode_request(OP_READ, 1, 0, 0, 1024))).unwrap();
    assert_eq!(run(d.recv_max(64)).unwrap().len(), 64, "the header: a short packet");
    assert!(matches!(run(d.recv_max(2048)), Err(FbError::Timeout(_))), "1024 bytes left, 2048 asked");
}

/// The wire format, pinned: the same bytes are in the RAM environment's unit test
/// (GlassineOS/images `ramenv/test/unit.c`), so this reader and the device's daemon
/// agree on them without a device.
#[test]
fn wire_format_pinned() {
    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
    }
    let request = unhex("4752525101000200040302010200000000100000000000000002000000000000");
    assert_eq!(encode_request(OP_READ, 0x0102_0304, 2, 0x1000, 0x200).to_vec(), request);
    let response = unhex(concat!(
        "475252530100020004030201000000000002000000000000001000000000000",
        "0000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
    ));
    let h = parse_header(&response).unwrap();
    assert_eq!((h.op, h.tag, h.status, h.length, h.offset), (OP_READ, 0x0102_0304, 0, 0x200, 0x1000));
    assert_eq!(h.sha256.to_vec(), (0u8..32).collect::<Vec<_>>());
}
