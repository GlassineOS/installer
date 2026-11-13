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

mod common;
use common::ramsim::*;

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
