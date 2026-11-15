// SPDX-License-Identifier: GPL-2.0-or-later
//! `patch-boot-image` through the RAM environment: a kept Android system's
//! boot image, changed in place by [`crate::boot_patch`] so it mounts its own
//! slot's vendor partition on the A/B table.
//!
//! The executor reads the partition before anything is written and keeps
//! that original on the computer; the change is always made to the
//! original, never to what the partition holds now. A write cut half-way
//! leaves an image whose header id doesn't match its content, which the
//! change refuses: a resume takes the kept original again and writes the
//! same bytes.
//!
//! - The change refuses the image (not one it knows, the vendor check still
//!   on): nothing is written and the run goes on ([`OpError::goes_on`]);
//!   only Android stays unable to start.
//! - The image already has the change: nothing to do.
//! - Otherwise only the WRITE ranges where the changed image differs from
//!   the original are written (each read back), then the device's hash of
//!   the whole partition is compared with the changed image's.

use super::{Evidence, OpError, RangeHashes, check_ranges, hex_sha, ram_err};
use crate::boot_patch::{Patched, patch_vendor_slotselect};
use crate::fastboot::Transport;
use crate::glassine_write::{WRITE_MAX, Writer};

/// Writes the change of `original` (the partition's whole content as read
/// before the step first wrote) to the partition at `start`.
pub async fn run<T: Transport>(
    w: &mut Writer<T>,
    disk: u32,
    part: &str,
    start: u64,
    original: &[u8],
    max_hash: u64,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Evidence, OpError> {
    let mut ev = Evidence::new();
    ev.insert("partition".into(), part.into());
    ev.insert("original-sha256".into(), hex_sha(original));
    let new = match patch_vendor_slotselect(original) {
        Ok(Patched::Changed(b)) => b,
        Ok(Patched::AlreadyDone) => {
            ev.insert("found".into(), "the boot image already mounts its own slot's vendor partition".into());
            return Ok(ev);
        }
        Err(e) => {
            return Err(OpError::goes_on(format!(
                "{part}: {e}. Nothing was written: Android on this slot stays as it was and can't start on the new layout"
            )));
        }
    };
    let total = new.len() as u64;
    let chunk = WRITE_MAX.min(max_hash) as usize;
    let mut written = 0u64;
    let mut at = 0usize;
    while at < new.len() {
        let n = chunk.min(new.len() - at);
        if new[at..at + n] != original[at..at + n] {
            w.write(disk, start + at as u64, &new[at..at + n]).await.map_err(|e| ram_err(e, true))?;
            written += n as u64;
        }
        at += n;
        progress(at as u64, total);
    }
    let mut hashes = RangeHashes::new(max_hash);
    hashes.feed(&new);
    check_ranges(w, disk, start, total, max_hash, &hashes.finish()).await?;
    ev.insert("changed-sha256".into(), hex_sha(&new));
    ev.insert("written".into(), format!("{written} of {total} bytes, the rest unchanged"));
    ev.insert("read-back".into(), "the device's hashes match the changed image".into());
    Ok(ev)
}
