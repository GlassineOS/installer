// SPDX-License-Identifier: GPL-2.0-or-later
//! 004 FR-004: the check of a supplied file, on synthetic files only. The
//! signed images here are built by the test: a tiny ELF with a hash segment
//! whose certificate chain is hand-assembled DER (no keys, no signatures that
//! verify, no vendor bytes).

mod common;
use common::*;
use glassine_installer::format1::Description;
use glassine_installer::supplied::{self, DeviceValues, Verdict, qualcomm};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

// ---- DER ---------------------------------------------------------------------

fn der(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let n = content.len();
    if n < 0x80 {
        out.push(n as u8);
    } else if n < 0x100 {
        out.extend([0x81, n as u8]);
    } else {
        out.extend([0x82, (n >> 8) as u8, n as u8]);
    }
    out.extend_from_slice(content);
    out
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    der(0x30, &parts.concat())
}

fn attr(oid: &[u8], value: &str) -> Vec<u8> {
    der(0x31, &seq(&[der(0x06, oid), der(0x13, value.as_bytes())]))
}

const CN: &[u8] = &[0x55, 0x04, 0x03];
const OU: &[u8] = &[0x55, 0x04, 0x0b];

fn name(cn: &str, ous: &[&str]) -> Vec<u8> {
    let mut rdns = vec![attr(CN, cn)];
    rdns.extend(ous.iter().map(|o| attr(OU, o)));
    seq(&rdns)
}

/// A certificate's structure, with a dummy key and signature.
fn cert(issuer: &[u8], subject: &[u8], serial: u8) -> Vec<u8> {
    let alg = seq(&[der(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b]), der(0x05, &[])]);
    let validity = seq(&[der(0x17, b"260101000000Z"), der(0x17, b"360101000000Z")]);
    let spki = seq(&[alg.clone(), der(0x03, &[0, 1, 2, 3])]);
    let tbs = seq(&[
        der(0xa0, &der(0x02, &[2])),
        der(0x02, &[serial]),
        alg.clone(),
        issuer.to_vec(),
        validity,
        subject.to_vec(),
        spki,
    ]);
    seq(&[tbs, alg, der(0x03, &[0; 33])])
}

struct Chain {
    der: Vec<u8>,
    root: Vec<u8>,
}

fn chain(hw_id: &str, root_cn: &str) -> Chain {
    let root_name = name(root_cn, &[]);
    let ca_name = name("Attestation CA", &[]);
    let attest_name = name("Attestation", &["01 0000000000000003 SW_ID", &format!("02 {hw_id} HW_ID"), "07 0001 SHA256"]);
    let root = cert(&root_name, &root_name, 1);
    let der = [cert(&ca_name, &attest_name, 3), cert(&root_name, &ca_name, 2), root.clone()].concat();
    Chain { der, root }
}

// ---- the signed image --------------------------------------------------------

/// A hash segment: the 40-byte header, hashes, signatures and chains.
fn hash_segment(version: u32, chain: &[u8], padding: usize) -> Vec<u8> {
    let hashes = vec![0u8; 3 * 32];
    let sig = vec![0x5a; 256];
    let mut chain = chain.to_vec();
    chain.extend(std::iter::repeat_n(0xff, padding));
    let w: [u32; 10] = match version {
        5 => [
            0,
            5,
            0,
            0,
            (hashes.len() + sig.len() + chain.len()) as u32,
            hashes.len() as u32,
            !0,
            sig.len() as u32,
            !0,
            chain.len() as u32,
        ],
        _ => [
            0,
            version,
            0,
            0,
            (hashes.len() + sig.len() + chain.len()) as u32,
            hashes.len() as u32,
            0,
            sig.len() as u32,
            0,
            chain.len() as u32,
        ],
    };
    let mut out: Vec<u8> = w.iter().flat_map(|x| x.to_le_bytes()).collect();
    out.extend(hashes);
    out.extend(sig);
    out.extend(chain);
    out
}

/// An ELF with a header placeholder, the hash segment and one LOAD segment;
/// the file ends where the last segment does.
fn elf(wide: bool, seg: &[u8]) -> Vec<u8> {
    let (ehsize, phentsize) = if wide { (64usize, 56usize) } else { (52, 32) };
    let phnum = 3;
    let hash_at = 0x1000;
    let load = vec![0xa5u8; 0x200];
    let load_at = hash_at + seg.len().next_multiple_of(0x100);
    let mut b = vec![0u8; load_at + load.len()];
    b[..4].copy_from_slice(b"\x7fELF");
    b[4] = if wide { 2 } else { 1 };
    b[5] = 1;
    b[6] = 1;
    let put16 = |b: &mut Vec<u8>, at: usize, v: u16| b[at..at + 2].copy_from_slice(&v.to_le_bytes());
    let put32 = |b: &mut Vec<u8>, at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |b: &mut Vec<u8>, at: usize, v: u64| b[at..at + 8].copy_from_slice(&v.to_le_bytes());
    let phoff = ehsize;
    if wide {
        put64(&mut b, 0x20, phoff as u64);
        put16(&mut b, 0x34, ehsize as u16);
        put16(&mut b, 0x36, phentsize as u16);
        put16(&mut b, 0x38, phnum);
    } else {
        put32(&mut b, 0x1c, phoff as u32);
        put16(&mut b, 0x28, ehsize as u16);
        put16(&mut b, 0x2a, phentsize as u16);
        put16(&mut b, 0x2c, phnum);
    }
    let phdrs: [(u32, u32, usize, usize); 3] = [
        (0, 0x0700_0000, 0, ehsize + 3 * phentsize),
        (0, 0x0220_0000, hash_at, seg.len()),
        (1, 0x0000_0007, load_at, load.len()),
    ];
    for (i, (ty, flags, off, size)) in phdrs.into_iter().enumerate() {
        let at = phoff + i * phentsize;
        put32(&mut b, at, ty);
        if wide {
            put32(&mut b, at + 4, flags);
            put64(&mut b, at + 8, off as u64);
            put64(&mut b, at + 32, size as u64);
            put64(&mut b, at + 40, size as u64);
        } else {
            put32(&mut b, at + 4, off as u32);
            put32(&mut b, at + 16, size as u32);
            put32(&mut b, at + 20, size as u32);
            put32(&mut b, at + 24, flags);
        }
    }
    b[hash_at..hash_at + seg.len()].copy_from_slice(seg);
    b[load_at..].copy_from_slice(&load);
    b
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

const CHIP: &str = "000cc0e1";
const HW_ID: &str = "000CC0E100000000";
/// The Note Air's key hash in its description; the synthetic root has its own.
const DESC_PK: &str = "7be49b72f9e4337223ccb84d6eccca4e61ce16e3602ac2008cb18b75babe6d09";
const DESC_KNOWN: &str = "b3e409558677f9b280fed7851e8cc66f0079ad78ef0b07519a3a98d86c14d41b";

/// The Note Air's description with the synthetic root's key hash in place of
/// its own, and `known` as the known checksum.
fn noteair_for(root: &[u8], known: &str) -> Description {
    let pk = hex(&Sha256::digest(root));
    let t = noteair().replace(DESC_PK, &pk).replace(DESC_KNOWN, known);
    valid(&t)
}

fn programmer(wide: bool, version: u32, hw_id: &str, root_cn: &str) -> (Vec<u8>, Vec<u8>) {
    let c = chain(hw_id, root_cn);
    (elf(wide, &hash_segment(version, &c.der, 17)), c.root)
}

fn check(d: &Description, bytes: &[u8], device: Option<&DeviceValues>) -> Verdict {
    supplied::check(d, "emergency-programmer", bytes, device).unwrap().verdict
}

fn device(chip: &str, pk: &str) -> DeviceValues {
    BTreeMap::from([("chip".into(), chip.into()), ("pk_hash".into(), pk.into())])
}

const OTHER: &str = "0000000000000000000000000000000000000000000000000000000000000000";

// ---- the route's recognition -------------------------------------------------

#[test]
fn reads_the_chip_and_the_root_key_from_32_and_64_bit_images() {
    for wide in [false, true] {
        for version in [3, 5] {
            let (b, root) = programmer(wide, version, HW_ID, "Root CA");
            let s = qualcomm::read(&b).unwrap_or_else(|e| panic!("wide {wide} v{version}: {e}"));
            assert_eq!(s.chip, CHIP);
            assert_eq!(s.hw_id, HW_ID.to_ascii_lowercase());
            assert_eq!(s.root_sha256, hex(&Sha256::digest(&root)));
            assert_eq!(s.certificates, 3);
            assert_eq!(s.version, version);
        }
    }
}

#[test]
fn a_hash_segment_version_it_doesnt_read_is_not_a_programmer() {
    let c = chain(HW_ID, "Root CA");
    let b = elf(false, &hash_segment(6, &c.der, 0));
    assert!(qualcomm::read(&b).unwrap_err().contains("version 6"));
}

// ---- the verdicts ------------------------------------------------------------

#[test]
fn a_known_file() {
    let (b, root) = programmer(false, 5, HW_ID, "Root CA");
    let d = noteair_for(&root, &hex(&Sha256::digest(&b)));
    assert_eq!(check(&d, &b, None), Verdict::Known { verified: false });
    assert_eq!(check(&d, &b, None).sendable(), Some(false));
    // the device reports what the description names: still known
    let pk = d.supplied_files[0].matches["pk_hash"].clone();
    assert_eq!(check(&d, &b, Some(&device(CHIP, &pk))), Verdict::Known { verified: false });
}

#[test]
fn a_file_that_fits_but_isnt_known() {
    let (b, root) = programmer(true, 5, HW_ID, "Root CA");
    let d = noteair_for(&root, OTHER);
    match check(&d, &b, None) {
        Verdict::FitsUnknown { values } => {
            assert_eq!(values["chip"], CHIP);
            assert_eq!(values["pk_hash"], hex(&Sha256::digest(&root)));
        }
        v => panic!("{v:?}"),
    }
    assert_eq!(check(&d, &b, None).sendable(), Some(true));
}

#[test]
fn a_file_for_another_chip() {
    let (b, root) = programmer(false, 5, "0014D0E100000000", "Root CA");
    let d = noteair_for(&root, OTHER);
    match check(&d, &b, None) {
        Verdict::WrongChipOrKey {
            made_for, differs, reported, ..
        } => {
            assert_eq!(differs, ["chip"]);
            assert_eq!(made_for["chip"], "0014d0e1");
            assert_eq!(reported, None);
        }
        v => panic!("{v:?}"),
    }
}

#[test]
fn a_file_signed_with_another_key() {
    let (_, root) = programmer(false, 5, HW_ID, "Root CA");
    let (b, _) = programmer(false, 5, HW_ID, "Another root");
    let d = noteair_for(&root, OTHER);
    match check(&d, &b, None) {
        Verdict::WrongChipOrKey { differs, .. } => assert_eq!(differs, ["pk_hash"]),
        v => panic!("{v:?}"),
    }
    assert_eq!(check(&d, &b, None).sendable(), None);
}

/// 004 edge cases: a device that reports another chip or key gets nothing,
/// a known checksum included.
#[test]
fn the_devices_report_decides_first() {
    let (b, root) = programmer(false, 5, HW_ID, "Root CA");
    let d = noteair_for(&root, &hex(&Sha256::digest(&b)));
    let pk = d.supplied_files[0].matches["pk_hash"].clone();
    for (chip, key, differs) in [("0014d0e1", pk.as_str(), "chip"), (CHIP, OTHER, "pk_hash")] {
        match check(&d, &b, Some(&device(chip, key))) {
            Verdict::WrongChipOrKey {
                reported: Some(dev),
                differs: dif,
                ..
            } => {
                assert_eq!(dif, [differs]);
                assert_eq!(dev["chip"], chip);
            }
            v => panic!("{v:?}"),
        }
    }
    // upper case from a device is the same value
    assert_eq!(
        check(&d, &b, Some(&device(&CHIP.to_ascii_uppercase(), &pk.to_ascii_uppercase()))),
        Verdict::Known { verified: false }
    );
}

#[test]
fn garbage_is_not_a_programmer() {
    let d = valid(&noteair());
    let mut x: u32 = 12345;
    let noise: Vec<u8> = (0..4096)
        .map(|_| {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (x >> 16) as u8
        })
        .collect();
    for b in [&b""[..], b"\x7fELF", &noise, b"hello, world"] {
        assert!(matches!(check(&d, b, None), Verdict::NotAProgrammer { .. }), "{b:?}");
    }
    // an ELF without a hash segment
    let (mut b, _) = programmer(false, 5, HW_ID, "Root CA");
    let at = 52 + 32 + 24;
    b[at..at + 4].copy_from_slice(&0u32.to_le_bytes());
    match check(&d, &b, None) {
        Verdict::NotAProgrammer { why } => assert!(why.contains("no hash segment"), "{why}"),
        v => panic!("{v:?}"),
    }
}

#[test]
fn a_chain_that_isnt_one_is_not_a_programmer() {
    let d = valid(&noteair());
    // one certificate only, and a chain that doesn't end in a root
    let one = cert(&name("Root CA", &[]), &name("Attestation", &["02 000CC0E100000000 HW_ID"]), 1);
    let not_root = [one.clone(), one.clone()].concat();
    for c in [one, not_root] {
        let b = elf(false, &hash_segment(5, &c, 0));
        assert!(matches!(check(&d, &b, None), Verdict::NotAProgrammer { .. }));
    }
    // no HW_ID
    let root_name = name("Root CA", &[]);
    let c = [cert(&root_name, &name("Attestation", &[]), 2), cert(&root_name, &root_name, 1)].concat();
    match check(&d, &elf(true, &hash_segment(5, &c, 0)), None) {
        Verdict::NotAProgrammer { why } => assert!(why.contains("HW_ID"), "{why}"),
        v => panic!("{v:?}"),
    }
}

/// A file cut anywhere is never known or fitting, and never panics.
#[test]
fn a_truncated_file_never_fits() {
    let (b, root) = programmer(false, 5, HW_ID, "Root CA");
    let d = noteair_for(&root, OTHER);
    assert!(matches!(check(&d, &b, None), Verdict::FitsUnknown { .. }));
    for n in 0..b.len() {
        let v = check(&d, &b[..n], None);
        assert!(matches!(v, Verdict::NotAProgrammer { .. }), "cut at {n}: {v:?}");
    }
    let (b, root) = programmer(true, 3, HW_ID, "Root CA");
    let d = noteair_for(&root, OTHER);
    for n in (0..b.len()).step_by(7) {
        assert!(matches!(check(&d, &b[..n], None), Verdict::NotAProgrammer { .. }), "cut at {n}");
    }
}

/// A file with every byte flipped in turn never panics.
#[test]
fn a_corrupted_file_never_panics() {
    let (b, root) = programmer(false, 5, HW_ID, "Root CA");
    let d = noteair_for(&root, OTHER);
    for i in 0..b.len().min(0x1000 + 1200) {
        let mut c = b.clone();
        c[i] ^= 0xff;
        let _ = check(&d, &c, None);
    }
}

#[test]
fn the_record_says_what_was_checked() {
    let d = valid(&noteair());
    let c = supplied::check(&d, "emergency-programmer", b"x", None).unwrap();
    let v: serde_json::Value = serde_json::to_value(&c).unwrap();
    assert_eq!(v["supplied_check_format"], 1);
    assert_eq!(v["device"], "onyx-noteair");
    assert_eq!(v["route"], "emergency-download");
    assert_eq!(v["size"], 1);
    assert_eq!(v["sha256"], hex(&Sha256::digest(b"x")));
    assert_eq!(v["verdict"], "not-a-programmer");
    assert!(supplied::check(&d, "nope", b"x", None).is_err());
}

/// The verdict's fields never shadow the record's own (`device`, `file`).
#[test]
fn the_record_keeps_its_own_keys() {
    let d = valid(&noteair());
    let c = supplied::check(&d, "emergency-programmer", b"x", Some(&device("0014d0e1", OTHER))).unwrap();
    let s = serde_json::to_string(&c).unwrap();
    for k in ["\"device\":", "\"file\":", "\"reported\":", "\"made_for\":"] {
        assert_eq!(s.matches(k).count(), 1, "{k} in {s}");
    }
}
