// SPDX-License-Identifier: GPL-2.0-or-later
//! The bytes the installer puts on a PC's boot partition for its records
//! (GlassineOS/boot `docs/boot-partition.md`, format 1): both slots of the
//! loader record and a kernel set's `set.rec`, compared with golden files,
//! and their check lines and the set's id worked out here independently.

mod common;

use common::golden;
use glassine_installer::pc_boot::{self, Content, KernelSet};
use sha2::{Digest, Sha256};

fn sha(b: &[u8]) -> String {
    Sha256::digest(b).iter().map(|x| format!("{x:02x}")).collect()
}

const SYSTEM: &str = "0F3E26A1-5C7B-4A55-9B8D-2C4F6E8A1B3D";
const USERDATA: &str = "6d1a5e2b-8c4f-4b3a-a9e7-1f2d3c4b5a69";

fn sets() -> (KernelSet, KernelSet) {
    let set = KernelSet::new(
        (&sha(b"kernel"), 12_345_678),
        Some((&sha(b"initrd"), 34_567_890)),
        "console=ttyS0 quiet",
        SYSTEM,
        USERDATA,
        "6.18.63-0-lts",
        "0123456789ab@00000000deadbeef",
    )
    .unwrap();
    let rec = KernelSet::new((&sha(b"recovery"), 23_456_789), None, "quiet", SYSTEM, USERDATA, "6.18.63-0-lts", "").unwrap();
    (set, rec)
}

#[test]
fn the_records_are_the_golden_bytes() {
    let (set, rec) = sets();
    let r = (rec.clone(), "recovery".to_string(), None);
    let f = pc_boot::files(
        "x86-64",
        ("boot-loader", 1),
        &set,
        "kernel",
        Content::Artifact("initramfs".into()),
        Some(&r),
        None,
    )
    .unwrap();
    let n = f.files.len();
    let slot = |i: usize| match &f.files[i].1 {
        Content::Bytes(b) => String::from_utf8(b.clone()).unwrap(),
        c => panic!("{c:?}"),
    };
    assert_eq!(f.files[n - 2].0.path, "glassine/loader.a");
    assert_eq!(f.files[n - 1].0.path, "glassine/loader.b");
    let (a, b) = (slot(n - 2), slot(n - 1));
    assert_eq!(a.len(), 16384);
    golden("core/tests/golden/pc-boot/loader.a", &a);
    golden("core/tests/golden/pc-boot/loader.b", &b);
    golden("core/tests/golden/pc-boot/set.rec", &set.record());
    golden("core/tests/golden/pc-boot/recovery-set.rec", &rec.record());

    // worked out independently of the installer's code
    let id = &sha(format!("{}\n{}\nconsole=ttyS0 quiet\n", sha(b"kernel"), sha(b"initrd")).as_bytes())[..16];
    assert_eq!(set.id, id);
    assert_eq!(&rec.id, &sha(format!("{}\n\nquiet\n", sha(b"recovery")).as_bytes())[..16]);
    let sr = set.record();
    let body = &sr[..sr.rfind("check=").unwrap()];
    assert_eq!(sr, format!("{body}check={}\n", sha(body.as_bytes())));
    let rec_text = a.trim_end_matches('\n');
    let lbody = &rec_text[..rec_text.rfind("check=").unwrap()];
    assert_eq!(&rec_text[lbody.len()..], format!("check={}", sha(lbody.as_bytes())));
    assert!(lbody.contains(&format!("default={id}\ndefault_rec={}\n", sha(sr.as_bytes()))));
    assert!(sr.contains(&format!(
        "cmdline=console=ttyS0 quiet glassine.system=PARTUUID={} glassine.userdata=PARTUUID={USERDATA} glassine.img={id}\n",
        SYSTEM.to_ascii_lowercase()
    )));
}
