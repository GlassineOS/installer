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

/// The attributes the installer gives the variables it writes: non-volatile,
/// boot-service and runtime access, as the firmware's own boot entries have.
pub const ATTRS_NV_BS_RT: u32 = 0x07;

/// Where a firmware boot entry starts its program: a GPT partition (as a
/// short-form `HD()` node, which every UEFI firmware expands to the disk it
/// finds the partition on) and a file on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HardDrive {
    /// The partition's number in the table (its slot, from 1).
    pub number: u32,
    /// Its first logical block and length in blocks, of the disk's sector size.
    pub start: u64,
    pub blocks: u64,
    /// Its unique partition GUID, on-disk byte order.
    pub guid: [u8; 16],
}

/// The bytes of an efivarfs file holding a `Boot####` load option: the
/// attributes, the option's own attributes (active), the path's length,
/// the label (UTF-16, ended by a zero), the device path, no optional data.
pub fn load_option_file(label: &str, hd: &HardDrive, file: &str) -> Vec<u8> {
    let mut path = Vec::new();
    path.extend([4u8, 1, 42, 0]);
    path.extend(hd.number.to_le_bytes());
    path.extend(hd.start.to_le_bytes());
    path.extend(hd.blocks.to_le_bytes());
    path.extend(hd.guid);
    path.push(2); // a GPT partition
    path.push(2); // its signature is a GUID
    let f: Vec<u8> = file.encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes()).collect();
    path.extend([4u8, 4]);
    path.extend(((4 + f.len()) as u16).to_le_bytes());
    path.extend(f);
    path.extend([0x7f, 0xff, 4, 0]);
    let mut v = ATTRS_NV_BS_RT.to_le_bytes().to_vec();
    v.extend(1u32.to_le_bytes()); // LOAD_OPTION_ACTIVE
    v.extend((path.len() as u16).to_le_bytes());
    v.extend(label.encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes()));
    v.extend(path);
    v
}

/// The bytes of an efivarfs file holding a list of boot entry numbers
/// (`BootOrder`), with the installer's attributes.
pub fn numbers_file(numbers: &[u16]) -> Vec<u8> {
    let mut v = ATTRS_NV_BS_RT.to_le_bytes().to_vec();
    for n in numbers {
        v.extend(n.to_le_bytes());
    }
    v
}

/// The list of numbers a `BootOrder` file holds.
pub fn number_list(file: &[u8]) -> Vec<u16> {
    value(file)
        .map(|v| v.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect())
        .unwrap_or_default()
}

/// The name of the efivarfs file of a global variable.
pub fn global_name(name: &str) -> String {
    format!("{name}-{GLOBAL}")
}

/// The name of the `Boot####` variable of `number`.
pub fn boot_name(number: u16) -> String {
    global_name(&format!("Boot{number:04X}"))
}

/// The number of a `Boot####` efivarfs file name, if it is one.
pub fn boot_number(file_name: &str) -> Option<u16> {
    let base = file_name.strip_suffix(&format!("-{GLOBAL}"))?;
    let n = base.strip_prefix("Boot")?;
    if n.len() != 4 || !n.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(n, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_built_load_option_reads_back() {
        let hd = HardDrive {
            number: 5,
            start: 2048,
            blocks: 4096,
            guid: [0x11; 16],
        };
        let f = load_option_file("GlassineOS", &hd, "\\EFI\\glassine\\loader.efi");
        let o = load_option(&f).unwrap();
        assert!(o.active);
        assert_eq!(o.label, "GlassineOS");
        assert_eq!(o.file.as_deref(), Some("\\EFI\\glassine\\loader.efi"));
        assert_eq!(o.partition_guid.as_deref(), Some(crate::table::guid(&[0x11; 16]).as_str()));
        assert_eq!(number_list(&numbers_file(&[3, 0x1000, 1])), vec![3, 0x1000, 1]);
        assert_eq!(boot_number(&boot_name(0x00af)), Some(0xaf));
        assert_eq!(boot_number(&global_name("BootOrder")), None);
    }
}
