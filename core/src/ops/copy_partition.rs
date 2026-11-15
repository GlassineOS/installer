// SPDX-License-Identifier: GPL-2.0-or-later
//! `copy-partition`: one partition of the device to another of the same
//! size, through the RAM environment: READs of the source, WRITEs of at most
//! 16 MiB to the target each checked by the device's read-back, then the
//! device's hash of every range of the target compared with what was read.

use super::{Evidence, Gpt, OpError, RangeHashes, check_ranges, ram_err};
use crate::fastboot::Transport;
use crate::glassine_write::{WRITE_MAX, Writer};
use sha2::{Digest, Sha256};

#[allow(clippy::too_many_arguments)]
pub async fn run<T: Transport>(
    w: &mut Writer<T>,
    disk: u32,
    gpt: &Gpt,
    from: &str,
    to: &str,
    max_hash: u64,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Evidence, OpError> {
    let (src, n) = gpt.extent(from).ok_or_else(|| OpError::before(format!("the partition table has no {from}")))?;
    let (dst, m) = gpt.extent(to).ok_or_else(|| OpError::before(format!("the partition table has no {to}")))?;
    if n != m {
        return Err(OpError::before(format!("{from} is {n} bytes and {to} {m}: not copied")));
    }
    let chunk = WRITE_MAX.min(max_hash);
    let mut hashes = RangeHashes::new(max_hash);
    let mut whole = Sha256::new();
    let mut at = 0u64;
    while at < n {
        let k = chunk.min(n - at);
        let data = w.read_all(disk, src + at, k).await.map_err(|e| ram_err(e, at > 0))?;
        w.write(disk, dst + at, &data).await.map_err(|e| ram_err(e, true))?;
        hashes.feed(&data);
        whole.update(&data);
        at += k;
        progress(at, n);
    }
    let want = hashes.finish();
    check_ranges(w, disk, dst, n, max_hash, &want).await?;
    let mut ev = Evidence::new();
    ev.insert("from".into(), from.into());
    ev.insert("partition".into(), to.into());
    ev.insert("bytes".into(), n.to_string());
    ev.insert("sha256".into(), super::hex(&whole.finalize()));
    ev.insert("read-back".into(), format!("{} ranges of {max_hash} bytes match", want.len()));
    Ok(ev)
}
