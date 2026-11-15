//! `set-boot-slot` through the RAM environment: the slot flags live in
//! attribute bits of each slot's entry in the partition table. Which entry
//! and which bits, and the values a pinned, fallback and unbootable slot
//! get, are the description's ([`SlotFlags`], its `slot-flags` boot
//! setting). Both table copies are rewritten with only those bits changed
//! (the table builder refuses any other difference), the backup copy first.
//!
//! - `pin`: the slot gets `pinned`, every other slot's entry `fallback`.
//! - `unbootable`: the slot gets `unbootable`.

use super::{Evidence, OpError, gpt_from_backup, param, read_gpt};
use crate::fastboot::Transport;
use crate::format1::Step;
use crate::glassine_write::Writer;
use crate::gpt_build::{Change, Recipe, Rename, build};
use crate::slot_flags::SlotFlags;

/// `resumed`: the step was cut or failed while it wrote; a primary copy
/// torn by that is rebuilt from the backup copy (written first, checked),
/// with `mbr` (the backed-up disk's sector 0) in front.
#[allow(clippy::too_many_arguments)]
pub async fn run<T: Transport>(
    w: &mut Writer<T>,
    disk: u32,
    sector: u64,
    disk_bytes: u64,
    flags: &SlotFlags,
    st: &Step,
    resumed: bool,
    mbr: &[u8],
    max_hash: u64,
) -> Result<Evidence, OpError> {
    let slot = param(st, "slot")?;
    let flag = |k: &str| st.params.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
    let (pin, unbootable) = (flag("pin"), flag("unbootable"));
    if pin == unbootable {
        return Err(OpError::before(format!("step {}: either pin or unbootable", st.id)));
    }
    let mut ev = Evidence::new();
    let gpt = match read_gpt(w, disk, sector).await {
        Ok(g) => g,
        Err(e) if resumed => {
            let g = gpt_from_backup(w, disk, sector, disk_bytes, mbr)
                .await
                .map_err(|b| OpError::before(format!("the primary partition table: {e}; its backup copy: {b}")))?;
            ev.insert(
                "found".into(),
                format!("a primary copy cut while it was written ({e}): rebuilt from the backup copy"),
            );
            g
        }
        Err(e) => return Err(OpError::before(format!("the partition table on the device: {e}"))),
    };
    let target = flags.entry(slot);
    let t = gpt
        .part(&target)
        .ok_or_else(|| OpError::before(format!("the partition table has no {target}: no slot {slot}")))?;
    let mut renames = vec![];
    let mut set = |name: &str, old: u64, bits: u64| {
        renames.push(Rename {
            from: name.into(),
            to: name.into(),
            attributes: Some(format!("0x{:016x}", flags.with(old, bits))),
        })
    };
    if pin {
        set(&target, t.attributes, flags.pinned);
        for o in gpt.parts.iter().filter(|p| p.name != target && flags.slot_of(&p.name).is_some()) {
            set(&o.name, o.attributes, flags.fallback);
        }
    } else {
        set(&target, t.attributes, flags.unbootable);
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
    super::partition_table::write_table(w, disk, &built, max_hash, &mut ev).await?;
    ev.insert("changes".into(), built.describe().join("; "));
    Ok(ev)
}
