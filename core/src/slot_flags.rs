// SPDX-License-Identifier: GPL-2.0-or-later
//! Where a device keeps its A/B slot flags in the partition table, and their
//! values, from its description: the parameters of its `slot-flags` boot
//! setting (format 3). Nothing about one boot loader's scheme is written in
//! code; a set-boot-slot step in a glassine-ram mode reads them here.

use crate::format1::Description;

/// The description's slot-flag scheme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotFlags {
    /// The entry's name before and after the slot letter.
    prefix: String,
    suffix: String,
    /// The attribute bits that hold the flags.
    pub bits: u64,
    pub pinned: u64,
    pub fallback: u64,
    pub unbootable: u64,
}

const KEYS: [&str; 5] = ["entry", "bits", "pinned", "fallback", "unbootable"];

fn hex(key: &str, v: &str) -> Result<u64, String> {
    let h = v
        .strip_prefix("0x")
        .filter(|h| !h.is_empty() && h.len() <= 16)
        .ok_or_else(|| format!("{key} = {v:?} isn't a 64-bit mask in hex (0x...)"))?;
    u64::from_str_radix(h, 16).map_err(|_| format!("{key} = {v:?} isn't hex"))
}

impl SlotFlags {
    /// The scheme of `d`'s `slot-flags` boot setting. `Ok(None)`: it names
    /// none (no such setting, or one without parameters).
    pub fn of(d: &Description) -> Result<Option<SlotFlags>, String> {
        let Some(b) = d.boot_settings.iter().find(|b| b.kind == "slot-flags") else {
            return Ok(None);
        };
        if b.params.is_empty() {
            return Ok(None);
        }
        let get = |k: &str| {
            b.params
                .get(k)
                .and_then(|v| v.as_str())
                .ok_or_else(|| format!("boot setting {}: {k} is missing (a slot-flag scheme names {})", b.id, KEYS.join(", ")))
        };
        let entry = get("entry")?;
        let (prefix, suffix) = entry
            .split_once("{slot}")
            .filter(|(_, after)| !after.contains("{slot}"))
            .ok_or_else(|| format!("boot setting {}: entry {entry:?} needs {{slot}} once", b.id))?;
        let bits = hex("bits", get("bits")?)?;
        let at = |k: &str| -> Result<u64, String> {
            let v = hex(k, get(k)?)?;
            if v & !bits != 0 {
                return Err(format!("boot setting {}: {k} has bits outside bits", b.id));
            }
            Ok(v)
        };
        Ok(Some(SlotFlags {
            prefix: prefix.into(),
            suffix: suffix.into(),
            bits,
            pinned: at("pinned")?,
            fallback: at("fallback")?,
            unbootable: at("unbootable")?,
        }))
    }

    /// The entry of slot `slot`.
    pub fn entry(&self, slot: &str) -> String {
        format!("{}{slot}{}", self.prefix, self.suffix)
    }

    /// The slot whose entry `name` is, if it is one (a single lower-case letter).
    pub fn slot_of(&self, name: &str) -> Option<String> {
        let mid = name.strip_prefix(&self.prefix)?.strip_suffix(&self.suffix)?;
        (mid.len() == 1 && mid.chars().all(|c| c.is_ascii_lowercase())).then(|| mid.to_string())
    }

    /// `old` with the flag bits set to `flags`, every other bit kept.
    pub fn with(&self, old: u64, flags: u64) -> u64 {
        (old & !self.bits) | flags
    }
}
