// SPDX-License-Identifier: GPL-2.0-or-later
//! The artifact source: the manifest's roles, the hash checked before use
//! and while streaming, errors that name the role.

use glassine_installer::artifacts::{ArtifactError, ArtifactSource, Artifacts, MemorySource, plain_path};
use glassine_installer::lock::sha256_hex;
use glassine_installer::table::ReadAt;

fn source(files: &[(&str, &[u8])], manifest: &str) -> MemorySource {
    let mut s = MemorySource {
        origin: "folder test".into(),
        ..Default::default()
    };
    s.files.insert("release.json".into(), manifest.as_bytes().to_vec());
    for (n, b) in files {
        s.files.insert(n.to_string(), b.to_vec());
    }
    s
}

fn kernel() -> Vec<u8> {
    (0..3_000_000u32).map(|i| (i * 7 % 251) as u8).collect()
}

fn manifest(sha: &str, size: u64) -> String {
    format!(
        r#"{{"release_format": 1, "artifacts": {{
            "kernel": {{"file": "boot/kernel.img", "sha256": "{sha}", "size": {size}, "kind": "raw-image",
                        "min_partition_bytes": 67108864}},
            "base": {{"min_partition_bytes": 8589934592}}
        }}}}"#
    )
}

fn good() -> Artifacts<MemorySource> {
    let k = kernel();
    Artifacts::open(source(&[("boot/kernel.img", &k)], &manifest(&sha256_hex(&k), k.len() as u64))).unwrap()
}

#[test]
fn a_good_artifact_verifies_and_streams() {
    let mut a = good();
    assert_eq!(a.roles(), vec!["kernel".to_string()]);
    assert_eq!(a.manifest.min_partition_bytes("base"), Some(8_589_934_592));
    let mut seen = vec![];
    let art = a.verify("kernel", &mut |n| seen.push(n)).unwrap();
    assert_eq!(art.kind.as_deref(), Some("raw-image"));
    assert_eq!(*seen.last().unwrap(), 3_000_000);
    // Streamed in pieces, as a sender would, and read again from 0.
    let mut r = a.reader("kernel").unwrap();
    let mut buf = vec![0u8; 1 << 20];
    r.read_at(0, &mut buf).unwrap();
    r.read_at(0, &mut buf).unwrap();
    let mut at = 1 << 20;
    while at < 3_000_000 {
        let n = (3_000_000 - at).min(1 << 20) as usize;
        r.read_at(at, &mut buf[..n]).unwrap();
        at += n as u64;
    }
    assert_eq!(r.finish().unwrap().sha256, sha256_hex(&kernel()));
}

#[test]
fn a_changed_byte_fails_the_read_that_completes_the_file() {
    let k = kernel();
    let mut bad = k.clone();
    bad[2_999_999] ^= 1;
    let mut a = Artifacts::open(source(&[("boot/kernel.img", &bad)], &manifest(&sha256_hex(&k), k.len() as u64))).unwrap();
    let e = a.verify("kernel", &mut |_| {}).unwrap_err();
    assert!(matches!(&e, ArtifactError::HashMismatch { role, .. } if role == "kernel"), "{e:?}");
    assert!(e.to_string().starts_with("kernel (boot/kernel.img) has SHA-256"));
    let mut r = a.reader("kernel").unwrap();
    let mut buf = vec![0u8; 2_999_999];
    r.read_at(0, &mut buf).unwrap();
    let mut last = [0u8];
    assert!(r.read_at(2_999_999, &mut last).unwrap_err().contains("not the release's file"));
}

#[test]
fn a_reader_refuses_reads_out_of_order_or_past_the_end_and_unfinished_reads() {
    let mut a = good();
    let mut r = a.reader("kernel").unwrap();
    let mut buf = [0u8; 512];
    assert!(r.read_at(512, &mut buf).unwrap_err().contains("expected 0"));
    r.read_at(0, &mut buf).unwrap();
    let mut big = vec![0u8; 3_000_000];
    assert!(r.read_at(512, &mut big).unwrap_err().contains("past its end"));
    assert!(r.finish().unwrap_err().to_string().contains("only 512 of 3000000"));
}

#[test]
fn a_wrong_size_is_refused_before_anything_is_read() {
    let k = kernel();
    let mut a = Artifacts::open(source(&[("boot/kernel.img", &k)], &manifest(&sha256_hex(&k), k.len() as u64 + 1))).unwrap();
    let e = a.reader("kernel").err().unwrap();
    assert!(matches!(
        e,
        ArtifactError::SizeMismatch {
            stated: 3_000_001,
            found: 3_000_000,
            ..
        }
    ));
}

#[test]
fn missing_roles_files_and_fields_name_the_role() {
    let mut a = good();
    assert_eq!(
        a.verify("recovery", &mut |_| {}).unwrap_err().to_string(),
        "the release has no artifact for recovery"
    );
    assert!(a.artifact("base").unwrap_err().to_string().contains("entry for base: no file"));
    let k = kernel();
    let mut a = Artifacts::open(source(&[], &manifest(&sha256_hex(&k), 1))).unwrap();
    let e = a.reader("kernel").err().unwrap();
    assert!(matches!(&e, ArtifactError::Unreadable { role, .. } if role == "kernel"), "{e}");
    let a = Artifacts::open(source(&[], &manifest("ABC", 1))).unwrap();
    assert!(a.artifact("kernel").unwrap_err().to_string().contains("64 lower-case hex"));
}

#[test]
fn a_path_leaving_the_release_is_refused() {
    for p in ["../x", "/etc/passwd", "a/../b", "a//b", "C:x", "a\\b", "./a", ""] {
        assert!(!plain_path(p), "{p}");
    }
    assert!(plain_path("boot/kernel.img"));
    let m = r#"{"release_format":1,"artifacts":{"kernel":{"file":"../k","sha256":"00","size":1}}}"#;
    let a = Artifacts::open(source(&[], m)).unwrap();
    assert!(a.artifact("kernel").unwrap_err().to_string().contains("not a plain relative path"));
}

#[test]
fn a_missing_or_newer_manifest_is_refused() {
    let s = MemorySource {
        origin: "folder empty".into(),
        ..Default::default()
    };
    assert!(Artifacts::open(s).err().unwrap().to_string().starts_with("folder empty has no release.json"));
    let e = Artifacts::open(source(&[], r#"{"release_format": 2}"#)).err().unwrap();
    assert!(matches!(e, ArtifactError::BadManifest { .. }));
    // The older fixture (only least sizes) still reads.
    let t = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/releases/base-8gib.json")).unwrap();
    assert!(Artifacts::open(source(&[], &t)).unwrap().roles().is_empty());
}

#[test]
fn an_empty_artifact_verifies() {
    let m = format!(
        r#"{{"release_format":1,"artifacts":{{"x":{{"file":"x","sha256":"{}","size":0}}}}}}"#,
        sha256_hex(b"")
    );
    let mut a = Artifacts::open(source(&[("x", b"")], &m)).unwrap();
    a.verify("x", &mut |_| {}).unwrap();
    let _ = a.source().origin();
}
