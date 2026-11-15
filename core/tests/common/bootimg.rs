// SPDX-License-Identifier: GPL-2.0-or-later
//! A small synthetic Android boot image (header version 0, no vendor
//! bytes) with a device tree appended to its kernel, for the steps that
//! change a kept system's boot image: the same shape as tests/boot_patch.rs
//! builds, with a kernel of any size.

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

/// A device tree with /firmware/android/fstab/vendor whose `fsmgr_flags`
/// holds `flags` in a value space of 12 bytes.
fn dtb(flags: &[u8]) -> Vec<u8> {
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
    for n in ["firmware", "android", "fstab", "vendor"] {
        begin(&mut st, n);
    }
    prop(&mut st, 0, b"/dev/block/by-name/vendor\0", 26);
    // the stock length field: the whole 12-byte space, padded with zeros
    let mut v = flags.to_vec();
    v.resize(12, 0);
    prop(&mut st, 4, &v, 12);
    for _ in 0..5 {
        st.extend(be(2));
    }
    st.extend(be(9));
    let off_struct = 40u32 + 16;
    let off_strings = off_struct + st.len() as u32;
    let total = off_strings + strings.len() as u32;
    let mut out = Vec::new();
    for x in [
        0xd00d_feed,
        total,
        off_struct,
        off_strings,
        40,
        17,
        16,
        0,
        strings.len() as u32,
        st.len() as u32,
    ] {
        out.extend(be(x));
    }
    out.extend([0u8; 16]);
    out.extend(st);
    out.extend(strings);
    out
}

/// A boot image whose vendor entry has `flags` (`wait` is changed,
/// `wait,verify` refused); its kernel is `kernel_len` bytes before the
/// device tree, so the tree lies that far into the image.
pub fn boot_image(flags: &[u8], kernel_len: usize) -> Vec<u8> {
    let mut kernel = vec![0x1f, 0x8b, 0x08, 0x00];
    kernel.extend((0..kernel_len as u32).map(|i| (i * 7) as u8));
    kernel.extend(dtb(flags));
    let ramdisk: Vec<u8> = (0..3000u32).map(|i| (i * 13) as u8).collect();
    let mut h = Sha1::new();
    for p in [&kernel[..], &ramdisk[..], &[][..]] {
        h.update(p);
        h.update((p.len() as u32).to_le_bytes());
    }
    let mut img = vec![0u8; PAGE];
    img[..8].copy_from_slice(b"ANDROID!");
    img[8..12].copy_from_slice(&(kernel.len() as u32).to_le_bytes());
    img[16..20].copy_from_slice(&(ramdisk.len() as u32).to_le_bytes());
    img[36..40].copy_from_slice(&(PAGE as u32).to_le_bytes());
    img[576..596].copy_from_slice(&h.finalize());
    for part in [&kernel, &ramdisk] {
        img.extend(part.iter());
        img.resize(img.len().div_ceil(PAGE) * PAGE, 0);
    }
    img
}
