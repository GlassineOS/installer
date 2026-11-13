// SPDX-License-Identifier: GPL-2.0-or-later
//! The Sahara session against a scripted boot ROM: the transfers it sends are
//! queued up front (the protocol is strictly question and answer, so the
//! order is all that matters), and everything the computer sends is kept and
//! compared. All values are synthetic: no serial, key hash or image of a real
//! unit.

use glassine_installer::fastboot::{FbError, Transport};
use glassine_installer::sahara::*;
use pollster::block_on;
use std::collections::VecDeque;

#[derive(Default)]
struct Rom {
    incoming: VecDeque<Vec<u8>>,
    sent: Vec<Vec<u8>>,
}

impl Rom {
    fn new(transfers: Vec<Vec<u8>>) -> Self {
        Rom {
            incoming: transfers.into(),
            sent: Vec::new(),
        }
    }
}

impl Transport for Rom {
    async fn send(&mut self, d: &[u8]) -> Result<(), FbError> {
        self.sent.push(d.to_vec());
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
        self.recv_max(256).await
    }

    // Like USB: one transfer at most `max` bytes; what doesn't fit stays for
    // the next one.
    async fn recv_max(&mut self, max: usize) -> Result<Vec<u8>, FbError> {
        let mut t = self.incoming.pop_front().ok_or(FbError::Timeout("no answer".into()))?;
        if t.len() > max {
            let rest = t.split_off(max);
            self.incoming.push_front(rest);
        }
        Ok(t)
    }
}

fn pkt(c: u32, body: &[u32]) -> Vec<u8> {
    let mut v = c.to_le_bytes().to_vec();
    v.extend_from_slice(&((8 + 4 * body.len()) as u32).to_le_bytes());
    for w in body {
        v.extend_from_slice(&w.to_le_bytes());
    }
    v
}

fn hello(mode: u32) -> Vec<u8> {
    hello_v(2, 1, mode)
}

fn hello_v(version: u32, min: u32, mode: u32) -> Vec<u8> {
    pkt(cmd::HELLO, &[version, min, 0x400, mode, 0, 0, 0, 0, 0, 0])
}

fn cmd_ready() -> Vec<u8> {
    pkt(cmd::CMD_READY, &[])
}

fn exec_resp(client: u32, len: u32) -> Vec<u8> {
    pkt(cmd::EXECUTE_RESP, &[client, len])
}

fn read32(image: u32, off: u32, len: u32) -> Vec<u8> {
    pkt(cmd::READ_DATA, &[image, off, len])
}

fn read64(image: u64, off: u64, len: u64) -> Vec<u8> {
    let mut v = cmd::READ_DATA_64.to_le_bytes().to_vec();
    v.extend_from_slice(&0x20u32.to_le_bytes());
    for q in [image, off, len] {
        v.extend_from_slice(&q.to_le_bytes());
    }
    v
}

fn end(image: u32, status: u32) -> Vec<u8> {
    pkt(cmd::END_IMAGE_TX, &[image, status])
}

fn done_resp(status: u32) -> Vec<u8> {
    pkt(cmd::DONE_RESP, &[status])
}

/// A synthetic image: bytes that differ at every offset.
fn image(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 + i / 251) as u8).collect()
}

const HW: u64 = 0x000c_c0e1_0051_0002;
const SERIAL: u32 = 0x1234_5678;

fn facts_script(sbl: Option<u32>) -> Vec<Vec<u8>> {
    let hash: Vec<u8> = (0u8..32).map(|b| b ^ 0x5a).collect();
    let mut s = vec![
        hello(0),
        cmd_ready(),
        exec_resp(1, 4),
        SERIAL.to_le_bytes().to_vec(),
        exec_resp(2, 8),
        HW.to_le_bytes().to_vec(),
        exec_resp(3, 96),
        [hash.clone(), hash.clone(), hash].concat(),
    ];
    match sbl {
        Some(v) => {
            s.push(exec_resp(7, 4));
            s.push(v.to_le_bytes().to_vec());
        }
        None => s.push(end(0, 0x1e)),
    }
    s.push(hello(0));
    s
}

#[test]
fn command_mode_reads_the_facts_and_switches_back() {
    let mut s = block_on(Sahara::connect(Rom::new(facts_script(Some(0x0000_0003))))).unwrap();
    assert_eq!(s.hello().version, 2);
    assert_eq!(s.hello().mode, Mode::ImageTxPending);
    let f = block_on(emergency_info(&mut s)).unwrap();
    assert_eq!(f.serial, SERIAL);
    assert_eq!(f.hw_id, HW);
    assert_eq!(
        f.hw(),
        HwId {
            msm_id: 0x000c_c0e1,
            oem_id: 0x0051,
            model_id: 0x0002
        }
    );
    assert_eq!(f.chip(), Some("SDM636"));
    assert_eq!(f.pk_hash().len(), 32, "the repeated hash folds to one copy");
    assert_eq!(f.pk_hash_raw.len(), 96);
    assert!(f.pk_hash_hex().starts_with("5a5b5859"));
    assert_eq!(f.sbl_version, Some(3));
    assert_eq!((f.sahara_version, f.sahara_version_min), (2, 1));
    // Back at a fresh HELLO.
    assert_eq!(s.hello().mode, Mode::ImageTxPending);
    let sent = &s.transport().sent;
    let mut want = vec![hello_resp(Mode::Command)];
    for c in [Exec::SerialNumber, Exec::MsmHwId, Exec::OemPkHash, Exec::SblVersion] {
        want.push(execute(c));
        want.push(execute_data(c));
    }
    want.push(switch_mode(Mode::Command));
    assert_eq!(sent, &want);
    // HELLO_RESP's layout: 0x30 bytes, version 2, oldest 1, status 0, mode 3.
    assert_eq!(sent[0].len(), 0x30);
    assert_eq!(&sent[0][..24], &pkt(cmd::HELLO_RESP, &[2, 1, 0, 3, 0, 0, 0, 0, 0, 0])[..24]);
}

#[test]
fn a_refused_sbl_version_is_none() {
    let mut s = block_on(Sahara::connect(Rom::new(facts_script(None)))).unwrap();
    let f = block_on(emergency_info(&mut s)).unwrap();
    assert_eq!(f.sbl_version, None);
    assert_eq!(f.serial, SERIAL);
}

#[test]
fn facts_debug_leaves_the_serial_out() {
    let mut s = block_on(Sahara::connect(Rom::new(facts_script(Some(1))))).unwrap();
    let f = block_on(emergency_info(&mut s)).unwrap();
    let d = format!("{f:?}");
    assert!(!d.contains(&SERIAL.to_string()) && !d.contains("12345678"), "{d}");
    assert!(d.contains("000cc0e100510002"));
}

#[test]
fn hw_id_decoding() {
    let h = HwId::from_u64(0x000c_c0e1_0000_0000);
    assert_eq!((h.msm_id, h.oem_id, h.model_id), (0x000c_c0e1, 0, 0));
    assert_eq!(h.chip(), Some("SDM636"));
    assert_eq!(HwId::from_u64(0x0009_00e1_0000_0000).chip(), None);
    assert_eq!(HwId::from_u64(u64::MAX).msm_id, u32::MAX);
}

#[test]
fn pk_hash_shapes() {
    let f = |raw: Vec<u8>| EmergencyFacts {
        sahara_version: 2,
        sahara_version_min: 1,
        serial: 0,
        hw_id: 0,
        pk_hash_raw: raw,
        sbl_version: None,
    };
    let h48: Vec<u8> = (0..48).collect();
    assert_eq!(f([h48.clone(), h48.clone()].concat()).pk_hash(), &h48[..]);
    assert_eq!(f(h48.clone()).pk_hash(), &h48[..], "one copy stays");
    let odd: Vec<u8> = (0..40).collect();
    assert_eq!(f(odd.clone()).pk_hash(), &odd[..], "an unknown shape stays as it came");
    let two: Vec<u8> = (0..64).collect();
    assert_eq!(f(two.clone()).pk_hash(), &two[..], "two different halves are not a repeat");
}

#[test]
fn upload_32_bit() {
    let img = image(10_000);
    let script = vec![
        hello(0),
        read32(13, 0, 52),
        read32(13, 52, 0x1000),
        read32(13, 9_000, 1_000),
        read32(13, 52, 0x1000), // asked again
        end(13, 0),
        done_resp(1),
    ];
    let mut s = block_on(Sahara::connect(Rom::new(script))).unwrap();
    let up = block_on(s.upload(&img)).unwrap();
    assert_eq!(
        up,
        Upload {
            image_id: 13,
            reads: 4,
            bytes: 52 + 0x1000 + 1_000 + 0x1000,
            wide: false,
            done: DoneStatus::Complete
        }
    );
    let sent = &s.transport().sent;
    assert_eq!(sent[0], hello_resp(Mode::ImageTxPending));
    assert_eq!(sent[1], img[..52]);
    assert_eq!(sent[2], img[52..52 + 0x1000]);
    assert_eq!(sent[3], img[9_000..]);
    assert_eq!(sent[4], img[52..52 + 0x1000]);
    assert_eq!(sent[5], done());
    assert_eq!(sent.len(), 6);
}

#[test]
fn upload_64_bit_split_and_glued_packets() {
    let img = image(3 << 20);
    let r1 = read64(13, 0, 0x50);
    let r2 = read64(13, 0x1000, (2 << 20) + 5); // larger than one send chunk
    let mut script = vec![hello(0)];
    // r1 split over three transfers, then r2 and END glued into one.
    script.push(r1[..3].to_vec());
    script.push(r1[3..20].to_vec());
    script.push(r1[20..].to_vec());
    script.push([r2, end(13, 0)].concat());
    script.push(done_resp(0));
    let mut s = block_on(Sahara::connect(Rom::new(script))).unwrap();
    let up = block_on(s.upload(&img)).unwrap();
    assert!(up.wide);
    assert_eq!(up.reads, 2);
    assert_eq!(up.done, DoneStatus::Pending);
    let sent = &s.transport().sent;
    assert_eq!(sent[1], img[..0x50]);
    // The big read went out in 1 MiB pieces, in order.
    let big: Vec<u8> = sent[2..sent.len() - 1].concat();
    assert_eq!(big, img[0x1000..0x1000 + (2 << 20) + 5]);
    assert!(sent[2..sent.len() - 1].iter().all(|p| p.len() <= SEND_CHUNK));
    assert_eq!(sent.last().unwrap(), &done());
}

fn upload_err(img: &[u8], script: Vec<Vec<u8>>) -> (SaharaError, Vec<Vec<u8>>) {
    let mut s = block_on(Sahara::connect(Rom::new(script))).unwrap();
    let e = block_on(s.upload(img)).unwrap_err();
    (e, s.into_transport().sent)
}

#[test]
fn out_of_range_reads_send_nothing() {
    let img = image(1000);
    for bad in [
        read32(13, 990, 11),
        read32(13, 1000, 1),
        read32(13, u32::MAX, u32::MAX),
        read32(13, 0, 0),
        read64(13, u64::MAX, 2),
        read64(13, 0, u64::MAX),
    ] {
        let (e, sent) = upload_err(&img, vec![hello(0), bad.clone()]);
        assert!(matches!(e, SaharaError::OutOfRange { image_len: 1000, .. }), "{e:?}");
        assert_eq!(sent, vec![hello_resp(Mode::ImageTxPending)], "nothing after HELLO_RESP for {bad:02x?}");
    }
}

#[test]
fn a_refused_image_is_a_nak() {
    let (e, sent) = upload_err(&image(100), vec![hello(0), read32(13, 0, 100), end(13, 0x25)]);
    assert_eq!(
        e,
        SaharaError::Nak {
            status: 0x25,
            during: "the image"
        }
    );
    assert!(e.to_string().contains("not signed"));
    assert_eq!(sent.len(), 2, "no DONE after a refusal");
}

#[test]
fn another_image_id_is_refused() {
    let (e, _) = upload_err(&image(100), vec![hello(0), read32(13, 0, 10), read32(7, 10, 10)]);
    assert_eq!(e, SaharaError::OtherImage { first: 13, now: 7 });
}

#[test]
fn a_looping_device_is_stopped() {
    let mut script = vec![hello(0)];
    script.extend(std::iter::repeat_n(read32(13, 0, 1 << 20), 6));
    let (e, sent) = upload_err(&image(1 << 20), script);
    assert!(matches!(e, SaharaError::TooManyReads { .. }), "{e:?}");
    assert_eq!(sent.len(), 1 + 5);
}

#[test]
fn done_must_be_answered() {
    let (e, _) = upload_err(&image(10), vec![hello(0), read32(13, 0, 10), end(13, 0), cmd_ready()]);
    assert_eq!(
        e,
        SaharaError::Unexpected {
            want: "DONE_RESP",
            got: "CMD_READY"
        }
    );
    let (e, _) = upload_err(&image(10), vec![hello(0), read32(13, 0, 10), end(13, 0)]);
    assert!(matches!(e, SaharaError::Transport(FbError::Timeout(_))), "{e:?}");
}

#[test]
fn malformed_packets() {
    // A HELLO cut short, then silence.
    let h = hello(0);
    let e = block_on(Sahara::connect(Rom::new(vec![h[..20].to_vec()]))).err().unwrap();
    assert_eq!(e, SaharaError::Short { want: 0x30, got: 20 });
    // Half a header.
    let e = block_on(Sahara::connect(Rom::new(vec![h[..5].to_vec()]))).err().unwrap();
    assert_eq!(e, SaharaError::Short { want: 8, got: 5 });
    // A zero-length transfer.
    let e = block_on(Sahara::connect(Rom::new(vec![vec![]]))).err().unwrap();
    assert_eq!(e, SaharaError::Short { want: 8, got: 0 });
    // A HELLO whose length field lies.
    let mut bad = h.clone();
    bad[4] = 0x2c;
    let e = block_on(Sahara::connect(Rom::new(vec![bad]))).err().unwrap();
    assert_eq!(e, SaharaError::BadLength { cmd: 1, len: 0x2c });
    // A length field of 4 GiB.
    let mut huge = h.clone();
    huge[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    let e = block_on(Sahara::connect(Rom::new(vec![huge]))).err().unwrap();
    assert_eq!(e, SaharaError::BadLength { cmd: 1, len: u32::MAX });
    // A command the protocol doesn't have.
    let e = block_on(Sahara::connect(Rom::new(vec![pkt(0x99, &[0, 0])]))).err().unwrap();
    assert_eq!(e, SaharaError::UnknownCommand(0x99));
    // A command only the computer sends.
    let e = block_on(Sahara::connect(Rom::new(vec![hello_resp(Mode::Command)]))).err().unwrap();
    assert_eq!(e, SaharaError::UnknownCommand(cmd::HELLO_RESP));
    // A valid packet first.
    let e = block_on(Sahara::connect(Rom::new(vec![cmd_ready()]))).err().unwrap();
    assert_eq!(
        e,
        SaharaError::Unexpected {
            want: "HELLO",
            got: "CMD_READY"
        }
    );
    // READ_DATA with the 64-bit length.
    let mut r = read32(13, 0, 4);
    r[4] = 0x20;
    let (e, _) = upload_err(&image(10), vec![hello(0), r]);
    assert_eq!(
        e,
        SaharaError::BadLength {
            cmd: cmd::READ_DATA,
            len: 0x20
        }
    );
}

#[test]
fn parse_packet_checks_the_buffer_too() {
    assert_eq!(parse_packet(&[1, 0, 0]), Err(SaharaError::Short { want: 8, got: 3 }));
    let h = hello(3);
    assert_eq!(parse_packet(&h[..0x2f]), Err(SaharaError::Short { want: 0x30, got: 0x2f }));
    assert_eq!(parse_packet(&[h.clone(), vec![0]].concat()), Err(SaharaError::Short { want: 0x30, got: 0x31 }));
    assert_eq!(
        parse_packet(&h),
        Ok(Packet::Hello(Hello {
            version: 2,
            version_min: 1,
            max_cmd_len: 0x400,
            mode: Mode::Command
        }))
    );
}

#[test]
fn unsupported_versions() {
    let e = block_on(Sahara::connect(Rom::new(vec![hello_v(3, 3, 0)]))).err().unwrap();
    assert_eq!(e, SaharaError::Unsupported { version: 3, version_min: 3 });
    let e = block_on(Sahara::connect(Rom::new(vec![hello_v(1, 2, 0)]))).err().unwrap();
    assert!(matches!(e, SaharaError::Unsupported { .. }));
    // A newer boot ROM that still speaks 2 is fine.
    assert!(block_on(Sahara::connect(Rom::new(vec![hello_v(3, 1, 0)]))).is_ok());
}

#[test]
fn command_mode_errors() {
    // The boot ROM refuses command mode.
    let mut s = block_on(Sahara::connect(Rom::new(vec![hello(0), end(0, 0x17)]))).unwrap();
    assert_eq!(
        block_on(s.enter_command_mode()),
        Err(SaharaError::Nak {
            status: 0x17,
            during: "command mode"
        })
    );
    // An answer for another command.
    let mut s = block_on(Sahara::connect(Rom::new(vec![hello(0), cmd_ready(), exec_resp(2, 8)]))).unwrap();
    block_on(s.enter_command_mode()).unwrap();
    assert!(matches!(block_on(s.execute(Exec::SerialNumber)), Err(SaharaError::BadAnswer(_))));
    // An answer too long to read.
    let mut s = block_on(Sahara::connect(Rom::new(vec![hello(0), cmd_ready(), exec_resp(1, 1 << 30)]))).unwrap();
    block_on(s.enter_command_mode()).unwrap();
    assert!(matches!(block_on(s.execute(Exec::SerialNumber)), Err(SaharaError::BadAnswer(_))));
    assert_eq!(s.transport().sent.len(), 2, "no EXECUTE_DATA for an answer that is refused");
    // An answer shorter than it said, then silence.
    let mut s = block_on(Sahara::connect(Rom::new(vec![hello(0), cmd_ready(), exec_resp(3, 32), vec![1; 10]]))).unwrap();
    block_on(s.enter_command_mode()).unwrap();
    assert_eq!(block_on(s.execute(Exec::OemPkHash)), Err(SaharaError::Short { want: 32, got: 10 }));
    // A serial of two bytes.
    let mut s = block_on(Sahara::connect(Rom::new(vec![hello(0), cmd_ready(), exec_resp(1, 2), vec![1, 2]]))).unwrap();
    block_on(s.enter_command_mode()).unwrap();
    assert!(matches!(block_on(s.read_facts()), Err(SaharaError::BadAnswer(_))));
    // A hardware id of four bytes.
    let mut s = block_on(Sahara::connect(Rom::new(vec![
        hello(0),
        cmd_ready(),
        exec_resp(1, 4),
        vec![0; 4],
        exec_resp(2, 4),
        vec![0; 4],
    ])))
    .unwrap();
    block_on(s.enter_command_mode()).unwrap();
    assert!(matches!(block_on(s.read_facts()), Err(SaharaError::BadAnswer(_))));
    // An empty key hash.
    let mut s = block_on(Sahara::connect(Rom::new(vec![
        hello(0),
        cmd_ready(),
        exec_resp(1, 4),
        vec![0; 4],
        exec_resp(2, 8),
        vec![0; 8],
        exec_resp(3, 0),
    ])))
    .unwrap();
    block_on(s.enter_command_mode()).unwrap();
    assert!(matches!(block_on(s.read_facts()), Err(SaharaError::BadAnswer(_))));
    // A HELLO where CMD_READY was due.
    let mut s = block_on(Sahara::connect(Rom::new(vec![hello(0), hello(0)]))).unwrap();
    assert_eq!(
        block_on(s.enter_command_mode()),
        Err(SaharaError::Unexpected {
            want: "CMD_READY",
            got: "HELLO"
        })
    );
}

#[test]
fn wrong_order_is_the_callers_mistake() {
    let mut s = block_on(Sahara::connect(Rom::new(vec![hello(0), cmd_ready()]))).unwrap();
    assert!(matches!(block_on(s.execute(Exec::SerialNumber)), Err(SaharaError::State(_))));
    assert!(matches!(block_on(s.switch_mode(Mode::ImageTxPending)), Err(SaharaError::State(_))));
    block_on(s.enter_command_mode()).unwrap();
    assert!(matches!(block_on(s.upload(&[1, 2, 3])), Err(SaharaError::State(_))));
    assert!(matches!(block_on(s.enter_command_mode()), Err(SaharaError::State(_))));
    assert_eq!(s.transport().sent, vec![hello_resp(Mode::Command)], "nothing sent for a refused call");
}

#[test]
fn reset_and_reset_state_machine() {
    let mut s = block_on(Sahara::connect(Rom::new(vec![hello(0), pkt(cmd::RESET_RESP, &[])]))).unwrap();
    block_on(s.reset()).unwrap();
    assert_eq!(s.transport().sent, vec![reset()]);
    assert_eq!(reset(), pkt(cmd::RESET, &[]));

    let mut s = block_on(Sahara::connect(Rom::new(vec![hello(0), end(0, 1)]))).unwrap();
    assert_eq!(block_on(s.reset()), Err(SaharaError::Nak { status: 1, during: "RESET" }));

    // After a refused read, a reset still works.
    let mut s = block_on(Sahara::connect(Rom::new(vec![hello(0), read32(13, 50, 100), pkt(cmd::RESET_RESP, &[])]))).unwrap();
    assert!(matches!(block_on(s.upload(&image(100))), Err(SaharaError::OutOfRange { .. })));
    block_on(s.reset()).unwrap();

    let mut s = block_on(Sahara::connect(Rom::new(vec![hello(0), hello(3)]))).unwrap();
    block_on(s.reset_state_machine()).unwrap();
    assert_eq!(s.transport().sent, vec![pkt(cmd::RESET_STATE_MACHINE, &[])]);
    assert_eq!(block_on(s.wait_hello()).unwrap().mode, Mode::Command);
}

#[test]
fn switch_mode_then_upload() {
    let img = image(64);
    let mut script = facts_script(Some(1));
    script.extend([read32(13, 0, 64), end(13, 0), done_resp(1)]);
    let mut s = block_on(Sahara::connect(Rom::new(script))).unwrap();
    block_on(emergency_info(&mut s)).unwrap();
    let up = block_on(s.upload(&img)).unwrap();
    assert_eq!(up.done, DoneStatus::Complete);
}

/// Arbitrary device bytes never panic: random transfers through connect,
/// command mode and upload, and through the parser.
#[test]
fn random_input_never_panics() {
    let mut x: u64 = 0x2545_f491_4f6c_dd1d;
    let mut rnd = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let valid = [
        hello(0),
        hello(3),
        cmd_ready(),
        exec_resp(1, 4),
        read32(13, 0, 8),
        read64(13, 4, 4),
        end(13, 0),
        done_resp(1),
    ];
    for _ in 0..3000 {
        let mut script = Vec::new();
        for _ in 0..(rnd() % 8) {
            let t = match rnd() % 3 {
                0 => valid[(rnd() % valid.len() as u64) as usize].clone(),
                1 => {
                    let mut v = valid[(rnd() % valid.len() as u64) as usize].clone();
                    let i = (rnd() as usize) % v.len();
                    v[i] = rnd() as u8;
                    v.truncate((rnd() as usize) % (v.len() + 1));
                    v
                }
                _ => (0..rnd() % 64).map(|_| rnd() as u8).collect(),
            };
            let _ = parse_packet(&t);
            script.push(t);
        }
        let img = image((rnd() % 32) as usize);
        if let Ok(mut s) = block_on(Sahara::connect(Rom::new(script))) {
            if rnd() % 2 == 0 {
                let _ = block_on(s.upload(&img));
            } else {
                let _ = block_on(emergency_info(&mut s));
            }
        }
    }
}
