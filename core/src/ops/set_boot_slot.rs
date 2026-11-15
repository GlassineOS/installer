// SPDX-License-Identifier: GPL-2.0-or-later
//! `set-boot-slot` through the RAM environment: the slot flags live in the
//! attribute bits 48 to 55 of the slot's boot partition entry (`boot_<slot>`,
//! the Qualcomm A/B convention the boot loader reads: priority 48-49, active
//! 50, tries 51-53, successful 54, unbootable 55). Both table copies are
//! rewritten with only those bits changed (the table builder refuses any
//! other difference), the backup copy first.
//!
//! - `pin`: the slot active, priority 3, 7 tries, successful (0x7f), and every
//!   other slot a lower-priority fallback that stays bootable (priority 1,
//!   active, 7 tries, successful: 0x7d). Seven tries cover GlassineOS's first
//!   start (a bare boot, a restart, the candidate's boot).
//! - `unbootable`: the slot inactive, priority 0, no tries, not successful,
//!   unbootable (0x80): the boot loader never picks it nor falls back to it.
//!
//! The values are the lab's slot tool's (set_active_slot.py), which a device
//! ran (slot switch and fallback, 2026-06-03, development).

use super::{Evidence, OpError, param, read_gpt};
use crate::fastboot::Transport;
use crate::format1::Step;
use crate::glassine_write::Writer;
use crate::gpt_build::{Change, Recipe, Rename, build};

pub const SLOT_BITS: u64 = 0xff << 48;
pub const PINNED: u64 = 0x7f << 48;
pub const FALLBACK: u64 = 0x7d << 48;
pub const RETIRED: u64 = 0x80 << 48;

/// The new attributes of `old` for the flag bits `bits`.
pub fn with_flags(old: u64, bits: u64) -> u64 {
    (old & !SLOT_BITS) | bits
}

pub async fn run<T: Transport>(w: &mut Writer<T>, disk: u32, sector: u64, disk_bytes: u64, st: &Step, max_hash: u64) -> Result<Evidence, OpError> {
    let slot = param(st, "slot")?;
    let flag = |k: &str| st.params.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let (pin, unbootable) = (flag("pin"), flag("unbootable"));
    if pin == unbootable {
        return Err(OpError::before(format!("step {}: either pin or unbootable", st.id)));
    }
    let gpt = read_gpt(w, disk, sector)
        .await
        .map_err(|e| OpError::before(format!("the partition table on the device: {e}")))?;
    let target = format!("boot_{slot}");
    let t = gpt
        .part(&target)
        .ok_or_else(|| OpError::before(format!("the partition table has no {target}: no slot {slot}")))?;
    let mut renames = vec![];
    let mut set = |name: &str, old: u64, bits: u64| {
        renames.push(Rename {
            from: name.into(),
            to: name.into(),
            attributes: Some(format!("0x{:016x}", with_flags(old, bits))),
        })
    };
    if pin {
        set(&target, t.attributes, PINNED);
        for o in gpt
            .parts
            .iter()
            .filter(|p| p.name.starts_with("boot_") && p.name.len() == 6 && p.name != target)
        {
            set(&o.name, o.attributes, FALLBACK);
        }
    } else {
        set(&target, t.attributes, RETIRED);
    }
    let recipe = Recipe {
        sector,
        entries: gpt.entries,
        align_sectors: 1,
        renames,
        shrink: None,
        place: Default::default(),
        new: vec![],
    };
    let built = build(&gpt.primary, Some(disk_bytes), &recipe).map_err(|e| OpError::before(format!("the slot flags: {e}")))?;
    if built.changes.iter().any(|c| matches!(c, Change::EntryArray { .. })) {
        return Err(OpError::before(
            "the table's geometry isn't what the builder writes: the slot flags aren't written",
        ));
    }
    let mut ev = Evidence::new();
    super::partition_table::write_table(w, disk, &built, max_hash, &mut ev).await?;
    ev.insert("changes".into(), built.describe().join("; "));
    Ok(ev)
}
