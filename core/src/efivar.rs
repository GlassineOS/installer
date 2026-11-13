// SPDX-License-Identifier: GPL-2.0-or-later
//! UEFI variables as efivarfs gives them (four attribute bytes, then the
//! value): Secure Boot's state, the boot order and the boot entries
//! (`Boot####`: a label and a device path).

use crate::table::{guid, le16, le32, utf16le};

/// The vendor GUID of the variables the UEFI specification defines.
pub const GLOBAL: &str = "8be4df61-93ca-11d2-aa0d-00e098032b8c";

/// The value of an efivarfs file: its bytes after the attributes.
pub fn value(file: &[u8]) -> Option<&[u8]> {
    file.get(4..)
}

/// A one-byte flag (SecureBoot, SetupMode).
pub fn flag(file: &[u8]) -> Option<bool> {
    value(file).and_then(|v| v.first()).map(|b| *b != 0)
}

/// A list of boot entry numbers (BootOrder), or one (BootCurrent).
pub fn numbers(file: &[u8]) -> Vec<String> {
    value(file)
        .map(|v| v.as_chunks::<2>().0.iter().map(|c| format!("{:04X}", u16::from_le_bytes(*c))).collect())
        .unwrap_or_default()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadOption {
    pub active: bool,
    pub label: String,
    /// The file the entry starts, as the firmware writes it (`\EFI\...`).
    pub file: Option<String>,
    /// The GPT unique GUID of the partition it starts from (an `HD()` node).
    /// Unique to the unit: used only to tell the disk the machine started from.
    pub partition_guid: Option<String>,
}

/// Parses a `Boot####` variable.
pub fn load_option(file: &[u8]) -> Option<LoadOption> {
    let v = value(file)?;
    if v.len() < 6 {
        return None;
    }
    let attrs = le32(v, 0);
    let path_len = le16(v, 4) as usize;
    let mut i = 6;
    let mut units = Vec::new();
    while i + 1 < v.len() {
        let u = le16(v, i);
        i += 2;
        if u == 0 {
            break;
        }
        units.push(u);
    }
    let label = String::from_utf16_lossy(&units);
    let path = v.get(i..i + path_len.min(v.len().saturating_sub(i)))?;
    let mut o = LoadOption {
        active: attrs & 1 != 0,
        label,
        ..Default::default()
    };
    let mut p = 0;
    while p + 4 <= path.len() {
        let (ty, sub, len) = (path[p], path[p + 1], le16(path, p + 2) as usize);
        if len < 4 || p + len > path.len() {
            break;
        }
        let node = &path[p..p + len];
        match (ty, sub) {
            (0x7f, 0xff) => break,
            // Media: hard drive, with a GPT signature.
            (4, 1) if len >= 42 && node[41] == 2 => o.partition_guid = Some(guid(&node[24..40])),
            // Media: file path.
            (4, 4) => {
                let f = String::from_utf16_lossy(&utf16le(&node[4..]));
                o.file = Some(match o.file.take() {
                    Some(prev) => format!("{prev}{f}"),
                    None => f,
                });
            }
            _ => {}
        }
        p += len;
    }
    Some(o)
}
