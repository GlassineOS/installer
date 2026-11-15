// SPDX-License-Identifier: GPL-2.0-or-later
//! The change a kept Android system's boot image needs on the A/B table
//! (002 FR-014b; the description's `patch-boot-image` step): the table step
//! renames `vendor` to `vendor_a`, and the stock boot image mounts its vendor
//! partition by the name in the vendor entry of the fstab in the kernel's
//! appended device tree. With `fsmgr_flags = "slotselect"` there, Android's
//! first stage adds its slot's suffix and finds `vendor_a`.
//!
//! The change is made in place and keeps the device tree's structure: the
//! property's value space (its length rounded up to 4 bytes) stays the same,
//! so no offset in the tree or the image moves. Only a value `wait` in a
//! 12-byte space (the space `wait,verify` had) takes `slotselect` that way;
//! anything else is refused. `wait,verify` itself is refused with its own
//! error: patching it would also switch off the check of the vendor
//! partition (dm-verity), which is the owner's decision, not this step's.
//!
//! Then the boot image header's id (SHA-1 over kernel, ramdisk and second
//! stage with their sizes) is recomputed, and a verified-boot 1.0 signature
//! after the payload, which signed the old bytes, is zeroed. Pure: bytes in,
//! bytes out, no device access.

use sha1::{Digest, Sha1};
use std::fmt;

/// The device-tree node of the vendor partition's fstab entry.
pub const VENDOR_NODE: &str = "/firmware/android/fstab/vendor";
/// The property the patch changes.
pub const FLAGS: &str = "fsmgr_flags";
/// The value it writes.
pub const SLOTSELECT: &str = "slotselect";
/// The value it changes.
pub const WAIT: &str = "wait";
/// The stock value it refuses.
pub const WAIT_VERIFY: &str = "wait,verify";

const MAGIC: &[u8; 8] = b"ANDROID!";
const FDT_MAGIC: u32 = 0xd00d_feed;
const ID_AT: usize = 576;
const ID_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchError {
    /// Not an Android boot image of header version 0.
    NotBootImage(String),
    /// The header's id doesn't match the image: something else changed it.
    IdMismatch,
    /// No single device tree appended to the kernel.
    NoDeviceTree(String),
    /// The device tree doesn't read as one.
    BadDeviceTree(String),
    /// No vendor fstab entry or no `fsmgr_flags` in it.
    NoVendorEntry,
    /// The vendor entry still has `wait,verify`: patching it would also
    /// switch off the vendor partition's check.
    VerifyStillOn,
    /// Another value, or a value space `slotselect` can't take in place.
    Unexpected(String),
    /// Something after the payload that isn't a verified-boot 1.0 signature.
    Trailer(String),
}

impl fmt::Display for PatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PatchError::NotBootImage(w) => write!(f, "not a boot image this step changes: {w}"),
            PatchError::IdMismatch => write!(f, "the boot image's header id doesn't match its content"),
            PatchError::NoDeviceTree(w) => write!(f, "no device tree appended to the kernel: {w}"),
            PatchError::BadDeviceTree(w) => write!(f, "the appended device tree doesn't read: {w}"),
            PatchError::NoVendorEntry => write!(f, "the device tree has no {VENDOR_NODE} with {FLAGS}"),
            PatchError::VerifyStillOn => write!(
                f,
                "the vendor entry's {FLAGS} is {WAIT_VERIFY:?}: changing it in place would also switch off the vendor partition's check (dm-verity), which this step doesn't decide"
            ),
            PatchError::Unexpected(w) => write!(f, "the vendor entry isn't one this step changes: {w}"),
            PatchError::Trailer(w) => write!(f, "after the boot image's payload: {w}"),
        }
    }
}

impl std::error::Error for PatchError {}

/// The result: the changed image (same length), or the image already has
/// the change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Patched {
    Changed(Vec<u8>),
    AlreadyDone,
}

fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u32be(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn align(n: usize, to: usize) -> usize {
    n.div_ceil(to) * to
}

/// The boot image's layout (header version 0).
struct Layout {
    kernel: (usize, usize),
    ramdisk: (usize, usize),
    second: (usize, usize),
    /// Where the payload ends, page-aligned.
    end: usize,
}

fn layout(img: &[u8]) -> Result<Layout, PatchError> {
    if img.len() < 2048 || &img[..8] != MAGIC {
        return Err(PatchError::NotBootImage("no ANDROID! header".into()));
    }
    let (ks, rs, ss) = (u32le(img, 8) as usize, u32le(img, 16) as usize, u32le(img, 24) as usize);
    let page = u32le(img, 36) as usize;
    let version = u32le(img, 40);
    if version != 0 {
        return Err(PatchError::NotBootImage(format!("header version {version}, not 0")));
    }
    if !page.is_power_of_two() || !(2048..=16384).contains(&page) {
        return Err(PatchError::NotBootImage(format!("page size {page}")));
    }
    let k = page;
    let r = k + align(ks, page);
    let s = r + align(rs, page);
    let end = s + align(ss, page);
    if ks == 0 || end > img.len() {
        return Err(PatchError::NotBootImage("its sizes don't fit the image".into()));
    }
    Ok(Layout {
        kernel: (k, ks),
        ramdisk: (r, rs),
        second: (s, ss),
        end,
    })
}

/// The header id of a version-0 boot image: SHA-1 over each part and its size.
fn id(img: &[u8], l: &Layout) -> [u8; 20] {
    let mut h = Sha1::new();
    for (at, n) in [l.kernel, l.ramdisk, l.second] {
        h.update(&img[at..at + n]);
        h.update((n as u32).to_le_bytes());
    }
    h.finalize().into()
}

/// Where the property's value is: (offset of its length field, offset of the value, length).
struct Prop {
    len_at: usize,
    value_at: usize,
    len: usize,
}

/// The device tree appended to the kernel: exactly one, ending where the kernel ends.
fn appended_dtb(kernel: &[u8]) -> Result<usize, PatchError> {
    let mut found = Vec::new();
    let mut i = 0;
    while let Some(p) = kernel[i..].windows(4).position(|w| w == FDT_MAGIC.to_be_bytes()) {
        let at = i + p;
        if let Some(total) = u32be(kernel, at + 4)
            && at + total as usize == kernel.len()
            && total >= 40
        {
            found.push(at);
        }
        i = at + 1;
    }
    match found.as_slice() {
        [one] => Ok(*one),
        [] => Err(PatchError::NoDeviceTree("no device tree ends where the kernel ends".into())),
        _ => Err(PatchError::NoDeviceTree(format!("{} candidates", found.len()))),
    }
}

fn cstr(b: &[u8], at: usize) -> Option<&str> {
    let rest = b.get(at..)?;
    let n = rest.iter().position(|&c| c == 0)?;
    std::str::from_utf8(&rest[..n]).ok()
}

/// Finds `prop` of the node at `path` in the device tree `dtb`.
fn find_prop(dtb: &[u8], path: &str, prop: &str) -> Result<Option<Prop>, PatchError> {
    let bad = |w: &str| PatchError::BadDeviceTree(w.to_string());
    let off_struct = u32be(dtb, 8).ok_or_else(|| bad("header"))? as usize;
    let off_strings = u32be(dtb, 12).ok_or_else(|| bad("header"))? as usize;
    let version = u32be(dtb, 20).ok_or_else(|| bad("header"))?;
    let size_strings = u32be(dtb, 32).ok_or_else(|| bad("header"))? as usize;
    let size_struct = u32be(dtb, 36).ok_or_else(|| bad("header"))? as usize;
    if version < 17 {
        return Err(bad(&format!("version {version}")));
    }
    let end = off_struct
        .checked_add(size_struct)
        .filter(|e| *e <= dtb.len())
        .ok_or_else(|| bad("structure block"))?;
    if off_strings.checked_add(size_strings).is_none_or(|e| e > dtb.len()) {
        return Err(bad("strings block"));
    }
    let strings = &dtb[off_strings..off_strings + size_strings];
    let mut stack: Vec<String> = Vec::new();
    let mut found = None;
    let mut at = off_struct;
    loop {
        if at + 4 > end {
            return Err(bad("structure block ends without FDT_END"));
        }
        let token = u32be(dtb, at).unwrap();
        at += 4;
        match token {
            1 => {
                let name = cstr(&dtb[..end], at).ok_or_else(|| bad("node name"))?.to_string();
                at = align(at + name.len() + 1, 4);
                stack.push(name);
            }
            2 => {
                stack.pop().ok_or_else(|| bad("unbalanced nodes"))?;
            }
            3 => {
                let len = u32be(dtb, at).ok_or_else(|| bad("property"))? as usize;
                let nameoff = u32be(dtb, at + 4).ok_or_else(|| bad("property"))? as usize;
                let value_at = at + 8;
                if value_at + len > end {
                    return Err(bad("property past the structure block"));
                }
                let here = format!("/{}", stack.iter().skip(1).cloned().collect::<Vec<_>>().join("/"));
                if here == path && cstr(strings, nameoff) == Some(prop) {
                    if found.is_some() {
                        return Err(bad("the property twice"));
                    }
                    found = Some(Prop { len_at: at, value_at, len });
                }
                at = align(value_at + len, 4);
            }
            4 => {}
            9 => break,
            t => return Err(bad(&format!("token {t:#x}"))),
        }
    }
    if !stack.is_empty() {
        return Err(bad("unbalanced nodes"));
    }
    Ok(found)
}

/// Makes the boot image's vendor fstab entry select its slot's partition
/// (see the module). The result has the input's length.
pub fn patch_vendor_slotselect(img: &[u8]) -> Result<Patched, PatchError> {
    let l = layout(img)?;
    if img[ID_AT..ID_AT + 20] != id(img, &l) || img[ID_AT + 20..ID_AT + ID_LEN].iter().any(|&b| b != 0) {
        return Err(PatchError::IdMismatch);
    }
    let (k, ks) = l.kernel;
    let dtb_at = k + appended_dtb(&img[k..k + ks])?;
    let dtb = &img[dtb_at..k + ks];
    let p = find_prop(dtb, VENDOR_NODE, FLAGS)?.ok_or(PatchError::NoVendorEntry)?;
    let value = &dtb[p.value_at..p.value_at + p.len];
    let text_len = value
        .iter()
        .position(|&c| c == 0)
        .ok_or_else(|| PatchError::Unexpected(format!("{FLAGS} isn't a string")))?;
    let text = String::from_utf8_lossy(&value[..text_len]).to_string();
    let space = align(p.len, 4);
    let padding_clean = dtb[p.value_at + text_len..p.value_at + space].iter().all(|&c| c == 0);
    if text == SLOTSELECT && p.len == SLOTSELECT.len() + 1 && padding_clean {
        return Ok(Patched::AlreadyDone);
    }
    if text == WAIT_VERIFY {
        return Err(PatchError::VerifyStillOn);
    }
    if text != WAIT {
        return Err(PatchError::Unexpected(format!("{FLAGS} is {text:?}, not {WAIT:?}")));
    }
    if space != align(SLOTSELECT.len() + 1, 4) || !padding_clean {
        return Err(PatchError::Unexpected(format!(
            "{FLAGS} has {} bytes of space, and {SLOTSELECT:?} needs {} in place",
            space,
            align(SLOTSELECT.len() + 1, 4)
        )));
    }
    // What follows the payload: nothing, or a verified-boot 1.0 signature
    // (one DER sequence) that signed the old bytes.
    let tail = &img[l.end..];
    let sig_len = match tail {
        t if t.iter().all(|&b| b == 0) => 0,
        [0x30, 0x82, hi, lo, ..] => {
            let n = 4 + ((*hi as usize) << 8 | *lo as usize);
            if n > tail.len() || tail[n..].iter().any(|&b| b != 0) {
                return Err(PatchError::Trailer("a sequence followed by other bytes".into()));
            }
            n
        }
        _ => return Err(PatchError::Trailer("bytes that aren't a verified-boot 1.0 signature".into())),
    };

    let mut out = img.to_vec();
    let base = dtb_at;
    out[base + p.len_at..base + p.len_at + 4].copy_from_slice(&((SLOTSELECT.len() + 1) as u32).to_be_bytes());
    let v = base + p.value_at;
    out[v..v + space].fill(0);
    out[v..v + SLOTSELECT.len()].copy_from_slice(SLOTSELECT.as_bytes());
    out[l.end..l.end + sig_len].fill(0);
    let new_id = id(&out, &l);
    out[ID_AT..ID_AT + 20].copy_from_slice(&new_id);
    Ok(Patched::Changed(out))
}
