// SPDX-License-Identifier: GPL-2.0-or-later
//! `write-image`: an artifact as a raw image to one partition.
//!
//! - Through a boot loader ([`flash`]): one `flash_raw` of the artifact,
//!   checked against the mode's [`WriteRules`] before anything is sent.
//!   Fastboot can't read back; the boot loader's OKAY is the evidence.
//! - Through the RAM environment ([`write_ram`]): WRITEs of at most 16 MiB
//!   streamed from the artifact (a tail that isn't a whole sector is padded
//!   with zeros), each checked by the device's read-back, then the device's
//!   hash of every range of max-hash bytes compared with what was written.
//!   Only the artifact's length is written; the rest of the partition keeps
//!   what it held.

use super::{Evidence, Gpt, OpError, RangeHashes, check_ranges, hex, ram_err};
use crate::artifacts::{ArtifactSource, Artifacts};
use crate::fastboot::{Session, Transport, WriteRules};
use crate::glassine_write::{WRITE_MAX, Writer};
use crate::table::ReadAt;

pub async fn flash<T: Transport, S: ArtifactSource>(
    s: &mut Session<T>,
    rules: &WriteRules,
    part: &str,
    role: &str,
    artifacts: &mut Artifacts<S>,
) -> Result<Evidence, OpError> {
    let mut r = artifacts.reader(role).map_err(|e| OpError::before(e.to_string()))?;
    let a = r.artifact.clone();
    match s.flash_raw(rules, part, &mut r, a.size).await {
        Ok(()) => {}
        Err(e) => {
            return Err(OpError {
                why: format!("{part}: {e}"),
                may_have_changed: e.partition_may_have_changed(),
                goes_on: false,
            });
        }
    }
    let mut ev = Evidence::new();
    ev.insert("partition".into(), part.into());
    ev.insert("artifact".into(), role.into());
    ev.insert("sha256".into(), a.sha256);
    ev.insert("bytes".into(), a.size.to_string());
    ev.insert("framing".into(), s.framing().name());
    ev.insert("answer".into(), "OKAY".into());
    Ok(ev)
}

/// Writes the artifact of `role` to partition `part` of `gpt` on `disk`.
#[allow(clippy::too_many_arguments)]
pub async fn write_ram<T: Transport, S: ArtifactSource>(
    w: &mut Writer<T>,
    disk: u32,
    gpt: &Gpt,
    part: &str,
    role: &str,
    artifacts: &mut Artifacts<S>,
    max_hash: u64,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Evidence, OpError> {
    let (start, size) = gpt.extent(part).ok_or_else(|| OpError::before(format!("the partition table has no {part}")))?;
    let mut r = artifacts.reader(role).map_err(|e| OpError::before(e.to_string()))?;
    let a = r.artifact.clone();
    let padded = a.size.div_ceil(gpt.sector) * gpt.sector;
    if a.size == 0 || padded > size {
        return Err(OpError::before(format!("{role} is {} bytes, {part} holds {size}: not written", a.size)));
    }
    let chunk = WRITE_MAX.min(max_hash);
    let mut hashes = RangeHashes::new(max_hash);
    let mut buf = vec![0u8; chunk as usize];
    let mut at = 0u64;
    while at < padded {
        let n = chunk.min(padded - at) as usize;
        let real = (a.size.saturating_sub(at)).min(n as u64) as usize;
        buf[..n].fill(0);
        r.read_at(at, &mut buf[..real]).map_err(|e| OpError {
            why: e,
            may_have_changed: at > 0,
            goes_on: false,
        })?;
        w.write(disk, start + at, &buf[..n]).await.map_err(|e| ram_err(e, true))?;
        hashes.feed(&buf[..n]);
        at += n as u64;
        progress(at, padded);
    }
    r.finish().map_err(|e| OpError::after(e.to_string()))?;
    let want = hashes.finish();
    check_ranges(w, disk, start, padded, max_hash, &want).await?;
    let mut ev = Evidence::new();
    ev.insert("partition".into(), part.into());
    ev.insert("artifact".into(), role.into());
    ev.insert("sha256".into(), a.sha256);
    ev.insert("bytes".into(), a.size.to_string());
    ev.insert("written".into(), format!("{padded} bytes at {start}"));
    ev.insert("read-back".into(), format!("{} ranges of {max_hash} bytes match", want.len()));
    if let Some(last) = want.last() {
        ev.insert("last-range-sha256".into(), hex(last));
    }
    Ok(ev)
}
