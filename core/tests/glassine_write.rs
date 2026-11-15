// SPDX-License-Identifier: GPL-2.0-or-later
//! The RAM environment's protocol version 2 client (`glassine_write`)
//! against the pinned bytes of GlassineOS/images `ramenv/test/unit.c` and
//! the simulated device of tests/common/ramsim.rs: arm, write with the
//! device's read-back, disarm, and every refusal and fault on the way.

mod common;
use common::ramsim::{RamEnv, Sparse, WriteFaults, image};
use glassine_installer::fastboot::Transport;
use glassine_installer::glassine_read::Reboot;
use glassine_installer::glassine_write::*;
use sha2::{Digest, Sha256};

fn block<F: Future>(f: F) -> F::Output {
    pollster::block_on(f)
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

const MIB: u64 = 1 << 20;

fn env() -> RamEnv {
    RamEnv::new(vec![
        ("mmcblk0".into(), Sparse::new(64 * MIB)),
        ("mmcblk0boot0".into(), Sparse::new(4 * MIB)),
    ])
}

fn range(offset: u64, length: u64) -> Range {
    Range { disk: 0, offset, length }
}

#[test]
fn the_write_request_and_answer_are_the_pinned_bytes() {
    let req = encode_request(OP_WRITE, 0x0102_0304, 0, 0x4400, 512);
    assert_eq!(req.to_vec(), unhex("4752525102000600040302010000000000440000000000000002000000000000"));
    let rsp = unhex(
        "4752525302000600040302010000000020000000000000000044000000000000000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
    );
    let h = parse_header(&rsp).unwrap();
    assert_eq!((h.op, h.tag, h.status, h.length, h.offset), (OP_WRITE, 0x0102_0304, 0, 32, 0x4400));
    assert_eq!(h.sha256.to_vec(), (0u8..32).collect::<Vec<_>>());
    // a version 1 answer is out of step for this client
    let mut v1 = rsp.clone();
    v1[4] = 1;
    assert!(matches!(parse_header(&v1), Err(RamError::OutOfStep(_))));
}

#[test]
fn arm_write_read_back_disarm() {
    let mut w = Writer::new(env());
    block(async {
        let l = w.list().await.unwrap();
        assert_eq!(l.writes, Writes::Off);
        assert_eq!(w.arm(&[range(0x4000, MIB), range(8 * MIB, 8 * MIB)]).await.unwrap(), 2);
        assert_eq!(w.list().await.unwrap().writes, Writes::Armed(2));
        let data = image(512, 7);
        let back = w.write(0, 0x4400, &data).await.unwrap();
        assert_eq!(back.as_slice(), Sha256::digest(&data).as_slice());
        assert_eq!(w.read(0, 0x4400, 512).await.unwrap(), data);
        let big = image(8 * MIB as usize, 8);
        w.write(0, 8 * MIB, &big).await.unwrap();
        assert_eq!(w.hash(0, 8 * MIB, 8 * MIB).await.unwrap().as_slice(), Sha256::digest(&big).as_slice());
        w.disarm().await.unwrap();
        assert_eq!(w.list().await.unwrap().writes, Writes::Off);
    });
    let d = w.into_transport();
    assert_eq!(d.written.len(), 2);
}

#[test]
fn the_device_refuses_what_the_contract_refuses() {
    let mut d = env();
    block(async {
        let mut w = Writer::new(&mut d);
        w.list().await.unwrap();
        // not armed: 6
        match w.write(0, 0, &[0; 512]).await {
            Err(RamError::Status { status: STATUS_NOT_ARMED, .. }) => {}
            r => panic!("{r:?}"),
        }
        // a boot area is never armed: refused before it is sent
        let boot = Range {
            disk: 1,
            offset: 0,
            length: 512,
        };
        assert!(matches!(w.arm(&[boot]).await, Err(RamError::Refused(_))));
        assert!(matches!(w.arm(&[range(0, 1024), range(512, 512)]).await, Err(RamError::Refused(_))));
        assert!(matches!(w.arm(&[range(0, 100)]).await, Err(RamError::Refused(_))));
        w.arm(&[range(0, MIB)]).await.unwrap();
        // outside the range this connection armed: never sent
        assert!(matches!(w.write(0, MIB, &[0; 512]).await, Err(RamError::Refused(_))));
        // ARM again: 8, even after DISARM
        match w.arm(&[range(MIB, MIB)]).await {
            Err(RamError::Status {
                status: STATUS_ALREADY_ARMED, ..
            }) => {}
            r => panic!("{r:?}"),
        }
        w.disarm().await.unwrap();
        match w.arm(&[range(MIB, MIB)]).await {
            Err(RamError::Status {
                status: STATUS_ALREADY_ARMED, ..
            }) => {}
            r => panic!("{r:?}"),
        }
    });
    // A refused ARM doesn't use the start up.
    let mut d = env();
    block(async {
        let raw = &mut d;
        let text = "range 0 0 1000\n";
        raw.send(&encode_request(OP_ARM, 1, 0, 0, text.len() as u64)).await.unwrap();
        raw.send(text.as_bytes()).await.unwrap();
        let h = parse_header(&raw.recv_max(64).await.unwrap()).unwrap();
        assert_eq!(h.status, 4);
        raw.recv_max(h.length as usize).await.unwrap();
        let mut w = Writer::new(&mut d);
        w.list().await.unwrap();
        w.arm(&[range(0, MIB)]).await.unwrap();
    });
    // A write across two armed ranges (found armed by another connection): 4.
    block(async {
        let mut w = Writer::new(&mut d);
        let l = w.list().await.unwrap();
        assert_eq!(l.writes, Writes::Armed(1));
        match w.write(0, MIB - 512, &[0; 1024]).await {
            Err(RamError::Status { status: 4, .. }) => {}
            r => panic!("{r:?}"),
        }
    });
    // Bytes whose sha256 isn't the one sent: 7, nothing written.
    block(async {
        let raw = &mut d;
        raw.send(&encode_request(OP_WRITE, 9, 0, 0, 512)).await.unwrap();
        raw.send(&[1; 512]).await.unwrap();
        raw.send(&[0; 32]).await.unwrap();
        let h = parse_header(&raw.recv_max(64).await.unwrap()).unwrap();
        assert_eq!(h.status, STATUS_BAD_DATA);
    });
    assert!(d.written.is_empty());
    assert_eq!(d.disks[0].1.read(0, 512), vec![0; 512]);
}

#[test]
fn a_read_back_that_differs_is_an_error() {
    let mut d = env();
    d.faults = WriteFaults {
        flip_write: Some(0),
        ..Default::default()
    };
    block(async {
        let mut w = Writer::new(&mut d);
        w.list().await.unwrap();
        w.arm(&[range(0, MIB)]).await.unwrap();
        match w.write(0, 0, &image(4096, 3)).await {
            Err(RamError::ReadBackDiffers { offset: 0, length: 4096, .. }) => {}
            r => panic!("{r:?}"),
        }
    });
}

#[test]
fn a_cut_write_is_a_transport_error() {
    let mut d = env();
    d.faults = WriteFaults {
        cut_write: Some(0),
        ..Default::default()
    };
    block(async {
        let mut w = Writer::new(&mut d);
        w.list().await.unwrap();
        w.arm(&[range(0, MIB)]).await.unwrap();
        assert!(matches!(w.write(0, 0, &image(4096, 3)).await, Err(RamError::Transport(_))));
    });
    // half of it landed
    assert_eq!(d.disks[0].1.read(0, 2048), image(4096, 3)[..2048]);
}

#[test]
fn reboot_disarms_and_v1_requests_get_v1_answers() {
    let mut d = env();
    block(async {
        let raw = &mut d;
        raw.send(&glassine_installer::glassine_read::encode_request(OP_LIST, 1, 0, 0, 0)).await.unwrap();
        let b = raw.recv_max(64).await.unwrap();
        assert_eq!(u16::from_le_bytes([b[4], b[5]]), 1);
        let n = u64::from_le_bytes(b[16..24].try_into().unwrap());
        let t = raw.recv_max(n as usize).await.unwrap();
        assert!(String::from_utf8(t).unwrap().starts_with("glassine-ramread 1\n"));
        let mut w = Writer::new(&mut d);
        w.list().await.unwrap();
        w.arm(&[range(0, MIB)]).await.unwrap();
        w.reboot(Reboot::BootLoader).await.unwrap();
    });
    assert_eq!(d.rebooted, Some(1));
    assert!(d.armed.is_none());
}
