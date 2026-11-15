// SPDX-License-Identifier: GPL-2.0-or-later
//! `write-partition-table` through the RAM environment: the new table built
//! from the unit's own primary table as the verified backup holds it and the
//! release's layout (the `partition-layout` artifact: a [`Recipe`] in JSON),
//! written backup copy first, read back, then the primary, read back. One
//! copy is whole at every moment: a cut between the two leaves the old
//! primary and the new backup, and the boot loader starts from the valid
//! primary.
//!
//! The layout must say what the step says: the same renames, the partitions
//! the step creates and resizes, the sizes its `partitions` fix. New
//! partitions without a GUID in the layout get one derived from the unit's
//! disk GUID ([`super::derived_guid`]), so a redo writes the same bytes.

use super::{Evidence, OpError, RangeHashes, check_ranges, derived_guid, parse_size, ram_err};
use crate::fastboot::Transport;
use crate::format1::Step;
use crate::glassine_write::{WRITE_MAX, Writer};
use crate::gpt_build::{Built, Change, Recipe, build};
use std::collections::BTreeSet;

/// The new table for this unit. `source` is its primary table (sector 0 to
/// the end of the entry array) from the verified backup.
pub fn layout(source: &[u8], disk_bytes: u64, recipe_json: &[u8], st: &Step) -> Result<Built, OpError> {
    let mut recipe: Recipe =
        serde_json::from_slice(recipe_json).map_err(|e| OpError::before(format!("the partition layout isn't one this installer reads: {e}")))?;
    let s = recipe.sector as usize;
    let disk_guid = source
        .get(s + 56..s + 72)
        .ok_or_else(|| OpError::before("the backed-up table is too short"))?
        .to_vec();
    for n in &mut recipe.new {
        if n.guid.is_empty() {
            n.guid = derived_guid(&disk_guid, &n.name);
        }
    }
    // The layout against the step.
    let bad = |w: String| OpError::before(format!("the partition layout doesn't match step {}: {w}", st.id));
    let mut want: BTreeSet<(String, String)> = BTreeSet::new();
    if let Some(t) = st.params.get("renames").and_then(|v| v.as_table()) {
        for (k, v) in t {
            want.insert((k.clone(), v.as_str().unwrap_or_default().to_string()));
        }
    }
    let got: BTreeSet<(String, String)> = recipe.renames.iter().map(|r| (r.from.clone(), r.to.clone())).collect();
    if want != got {
        return Err(bad(format!("renames {got:?}, the step {want:?}")));
    }
    let made: BTreeSet<&str> = recipe.new.iter().map(|n| n.name.as_str()).collect();
    let creates: BTreeSet<&str> = st.creates.iter().map(String::as_str).collect();
    if made != creates {
        return Err(bad(format!("creates {made:?}, the step {creates:?}")));
    }
    if let Some(sh) = &recipe.shrink
        && !st.resizes.contains(sh)
    {
        return Err(bad(format!("shrinks {sh}, which the step doesn't resize")));
    }
    let built = build(source, Some(disk_bytes), &recipe).map_err(|e| OpError::before(format!("the new partition table: {e}")))?;
    for p in st.params.get("partitions").and_then(|v| v.as_array()).into_iter().flatten() {
        let (Some(name), Some(size)) = (p.get("name").and_then(|v| v.as_str()), p.get("size").and_then(|v| v.as_str())) else {
            continue;
        };
        let fixed = parse_size(size).ok_or_else(|| bad(format!("{name}: size {size:?}")))?;
        let made = built.changes.iter().find_map(|c| match c {
            Change::Added { name: n, size_bytes, .. } if n == name => Some(*size_bytes),
            _ => None,
        });
        if made != Some(fixed) {
            return Err(bad(format!("{name} would be {made:?} bytes, the step fixes {fixed}")));
        }
    }
    Ok(built)
}

/// Writes `bytes` at `offset` in WRITEs of at most 16 MiB, then compares the
/// device's hashes of the range.
pub async fn write_checked<T: Transport>(w: &mut Writer<T>, disk: u32, offset: u64, bytes: &[u8], max_hash: u64) -> Result<(), OpError> {
    let mut h = RangeHashes::new(max_hash);
    for (k, c) in bytes.chunks(WRITE_MAX as usize).enumerate() {
        w.write(disk, offset + k as u64 * WRITE_MAX, c).await.map_err(|e| ram_err(e, true))?;
        h.feed(c);
    }
    check_ranges(w, disk, offset, bytes.len() as u64, max_hash, &h.finish()).await
}

/// Both copies of `built`: the backup copy first, read back, then the primary.
pub async fn write_table<T: Transport>(w: &mut Writer<T>, disk: u32, built: &Built, max_hash: u64, ev: &mut Evidence) -> Result<(), OpError> {
    write_checked(w, disk, built.backup_offset(), &built.backup, max_hash).await?;
    ev.insert(
        "backup-copy".into(),
        format!(
            "{} bytes at {}, sha256 {}",
            built.backup.len(),
            built.backup_offset(),
            super::hex_sha(&built.backup)
        ),
    );
    write_checked(w, disk, built.primary_offset(), &built.primary, max_hash).await?;
    ev.insert(
        "primary-copy".into(),
        format!("{} bytes at 0, sha256 {}", built.primary.len(), super::hex_sha(&built.primary)),
    );
    Ok(())
}

/// Writes the new table. The live primary must be the backed-up one or the
/// new one; anything else is refused unless the step was cut while it
/// wrote (`resumed`), when a torn primary is expected.
pub async fn run<T: Transport>(w: &mut Writer<T>, disk: u32, built: &Built, source: &[u8], resumed: bool, max_hash: u64) -> Result<Evidence, OpError> {
    let live = w.read_all(disk, 0, built.primary.len() as u64).await.map_err(|e| ram_err(e, false))?;
    let found = if live == built.primary {
        "the new table"
    } else if live.get(..source.len()) == Some(source) {
        "the backed-up table"
    } else if resumed {
        "a table cut while it was written"
    } else {
        return Err(OpError::before(
            "the partition table on the device is neither the backed-up one nor the new one: it changed after the backup",
        ));
    };
    let mut ev = Evidence::new();
    ev.insert("found".into(), found.into());
    write_table(w, disk, built, max_hash, &mut ev).await?;
    ev.insert("changes".into(), built.describe().join("; "));
    Ok(ev)
}
