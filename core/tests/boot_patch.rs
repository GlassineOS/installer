// SPDX-License-Identifier: GPL-2.0-or-later
//! The boot image change of the `patch-boot-image` step, on a small
//! synthetic boot image built here (no vendor bytes), and optionally on a
//! real pair: set GLASSINE_BOOT_PATCH_IN to a boot image and
//! GLASSINE_BOOT_PATCH_OUT to the image the change is known to give; the
//! test then checks the result byte for byte. Without them it says skipped.

use glassine_installer::boot_patch::{PatchError, Patched, patch_vendor_slotselect};
use sha1::{Digest, Sha1};

const PAGE: usize = 4096;

fn be(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

fn pad4(b: &mut Vec<u8>) {
    while !b.len().is_multiple_of(4) {
        b.push(0);
    }
}

/// A device tree with /firmware/android/fstab/vendor holding `dev` and,
/// unless `flags` is None, `fsmgr_flags` = (value, length field).
fn dtb(flags: Option<(&[u8], u32)>, node: &str) -> Vec<u8> {
    let strings = b"dev\0fsmgr_flags\0".to_vec();
    let mut st = Vec::new();
    let begin = |st: &mut Vec<u8>, name: &str| {
        st.extend(be(1));
        st.extend(name.as_bytes());
        st.push(0);
        pad4(st);
    };
    let prop = |st: &mut Vec<u8>, nameoff: u32, value: &[u8], len: u32| {
        st.extend(be(3));
        st.extend(be(len));
        st.extend(be(nameoff));
        st.extend(value);
        pad4(st);
    };
    begin(&mut st, "");
    for n in ["firmware", "android", "fstab", node] {
        begin(&mut st, n);
    }
    prop(&mut st, 0, b"/dev/block/by-name/vendor\0", 26);
    if let Some((v, len)) = flags {
        prop(&mut st, 4, v, len);
    }
    for _ in 0..5 {
        st.extend(be(2));
    }
    st.extend(be(9));
    let off_rsv = 40u32;
    let off_struct = off_rsv + 16;
    let off_strings = off_struct + st.len() as u32;
    let total = off_strings + strings.len() as u32;
    let mut out = Vec::new();
    for v in [
        0xd00d_feed,
        total,
        off_struct,
        off_strings,
        off_rsv,
        17,
        16,
        0,
        strings.len() as u32,
        st.len() as u32,
    ] {
        out.extend(be(v));
    }
    out.extend([0u8; 16]);
    out.extend(st);
    out.extend(strings);
    out
}

fn sha_id(kernel: &[u8], ramdisk: &[u8]) -> [u8; 20] {
    let mut h = Sha1::new();
    h.update(kernel);
    h.update((kernel.len() as u32).to_le_bytes());
    h.update(ramdisk);
    h.update((ramdisk.len() as u32).to_le_bytes());
    h.update([]);
    h.update(0u32.to_le_bytes());
    h.finalize().into()
}

/// A version-0 boot image: header page, kernel (a fake compressed kernel
/// plus the device tree), ramdisk, then `trailer` and zeros.
fn boot(dtb: &[u8], trailer: &[u8]) -> Vec<u8> {
    let mut kernel = vec![0x1f, 0x8b, 0x08, 0x00];
    kernel.extend((0..1000u32).map(|i| (i * 7) as u8));
    kernel.extend(dtb);
    let ramdisk: Vec<u8> = (0..3000u32).map(|i| (i * 13) as u8).collect();
    let mut img = vec![0u8; PAGE];
    img[..8].copy_from_slice(b"ANDROID!");
    img[8..12].copy_from_slice(&(kernel.len() as u32).to_le_bytes());
    img[16..20].copy_from_slice(&(ramdisk.len() as u32).to_le_bytes());
    img[36..40].copy_from_slice(&(PAGE as u32).to_le_bytes());
    img[576..596].copy_from_slice(&sha_id(&kernel, &ramdisk));
    for part in [&kernel, &ramdisk] {
        img.extend(part.iter());
        img.resize(img.len().div_ceil(PAGE) * PAGE, 0);
    }
    img.extend(trailer);
    img.resize(img.len() + 2 * PAGE, 0);
    img
}

fn signature() -> Vec<u8> {
    let body: Vec<u8> = (0..300u32).map(|i| (i * 31 + 1) as u8).collect();
    let mut s = vec![0x30, 0x82, (body.len() >> 8) as u8, body.len() as u8];
    s.extend(body);
    s
}

const WAIT12: &[u8] = b"wait\0\0\0\0\0\0\0\0";

#[test]
fn wait_becomes_slotselect_in_place() {
    let before = boot(&dtb(Some((WAIT12, 12)), "vendor"), &signature());
    let Patched::Changed(after) = patch_vendor_slotselect(&before).unwrap() else {
        panic!("expected a change")
    };
    assert_eq!(after.len(), before.len());
    // The value and its length changed; the space didn't move.
    let at = before.windows(WAIT12.len()).position(|w| w == WAIT12).unwrap();
    assert_eq!(&after[at - 8..at - 4], &11u32.to_be_bytes());
    assert_eq!(&after[at..at + 12], b"slotselect\0\0");
    // The signature is gone, the id matches the new content.
    let sig_at = 3 * PAGE;
    assert!(after[sig_at..].iter().all(|&b| b == 0));
    let diff: Vec<usize> = (0..before.len()).filter(|&i| before[i] != after[i]).collect();
    assert!(
        diff.iter().all(|&i| (576..596).contains(&i) || (at - 8..at + 12).contains(&i) || i >= sig_at),
        "{diff:?}"
    );
    // A second run has nothing to do.
    assert_eq!(patch_vendor_slotselect(&after).unwrap(), Patched::AlreadyDone);
}

#[test]
fn an_image_without_a_signature_is_changed_too() {
    let before = boot(&dtb(Some((WAIT12, 12)), "vendor"), &[]);
    assert!(matches!(patch_vendor_slotselect(&before), Ok(Patched::Changed(_))));
}

#[test]
fn stock_wait_verify_is_refused() {
    let img = boot(&dtb(Some((b"wait,verify\0", 12)), "vendor"), &signature());
    assert_eq!(patch_vendor_slotselect(&img), Err(PatchError::VerifyStillOn));
}

#[test]
fn a_value_space_too_small_is_refused() {
    let img = boot(&dtb(Some((b"wait\0", 5)), "vendor"), &[]);
    assert!(matches!(patch_vendor_slotselect(&img), Err(PatchError::Unexpected(_))));
}

#[test]
fn other_flags_are_refused() {
    let img = boot(&dtb(Some((b"wait,check\0\0", 12)), "vendor"), &[]);
    assert!(matches!(patch_vendor_slotselect(&img), Err(PatchError::Unexpected(_))));
}

#[test]
fn no_vendor_entry_is_refused() {
    let img = boot(&dtb(Some((WAIT12, 12)), "system"), &[]);
    assert_eq!(patch_vendor_slotselect(&img), Err(PatchError::NoVendorEntry));
    let img = boot(&dtb(None, "vendor"), &[]);
    assert_eq!(patch_vendor_slotselect(&img), Err(PatchError::NoVendorEntry));
}

#[test]
fn a_changed_header_id_is_refused() {
    let mut img = boot(&dtb(Some((WAIT12, 12)), "vendor"), &[]);
    img[580] ^= 1;
    assert_eq!(patch_vendor_slotselect(&img), Err(PatchError::IdMismatch));
}

#[test]
fn bytes_after_the_payload_that_are_no_signature_are_refused() {
    let img = boot(&dtb(Some((WAIT12, 12)), "vendor"), b"not a signature");
    assert!(matches!(patch_vendor_slotselect(&img), Err(PatchError::Trailer(_))));
}

#[test]
fn no_appended_device_tree_is_refused() {
    let mut d = dtb(Some((WAIT12, 12)), "vendor");
    d[0] = 0;
    let img = boot(&d, &[]);
    assert!(matches!(patch_vendor_slotselect(&img), Err(PatchError::NoDeviceTree(_))));
}

#[test]
fn not_a_boot_image_is_refused() {
    assert!(matches!(patch_vendor_slotselect(&[0u8; 8192]), Err(PatchError::NotBootImage(_))));
}

#[test]
fn a_known_pair_is_reproduced_byte_for_byte() {
    let (Ok(input), Ok(expected)) = (std::env::var("GLASSINE_BOOT_PATCH_IN"), std::env::var("GLASSINE_BOOT_PATCH_OUT")) else {
        eprintln!("skipped: GLASSINE_BOOT_PATCH_IN and GLASSINE_BOOT_PATCH_OUT not set");
        return;
    };
    let input = std::fs::read(input).unwrap();
    let expected = std::fs::read(expected).unwrap();
    let Patched::Changed(out) = patch_vendor_slotselect(&input).unwrap() else {
        panic!("expected a change")
    };
    assert!(out == expected, "the result differs from the known image");
    assert_eq!(patch_vendor_slotselect(&expected).unwrap(), Patched::AlreadyDone);
    eprintln!("reproduced byte for byte ({} bytes)", out.len());
}
