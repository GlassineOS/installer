// SPDX-License-Identifier: GPL-2.0-or-later
//! The full backup through the RAM environment (spec 001 FR-009), for a
//! device whose boot loader starts a boot image sent over USB.
//!
//! This is the one path that sends `download` and `boot`. Every other
//! fastboot session only reads (`getvar`) and restarts (`reboot`).
//!
//! The steps:
//! 1. A fresh inspect of the boot loader, in the framing its description
//!    tells ([`inspect::inspect_open`]).
//! 2. The RAM environment's image is downloaded and booted. On a layout with
//!    two slots, or where the description says booting an image changes the
//!    slot flags, only when the caller accepted that ([`BackupOptions`]).
//! 3. The RAM environment comes up as its own USB device: the description's
//!    `glassine-ram` mode names its ids.
//! 4. Every disk it lists is read whole into a stored copy, each chunk
//!    checked against the device's sha256. Then the device hashes every
//!    range again, the stored copy is read back and compared, and a
//!    manifest is written.
//! 5. The RAM environment reboots the device into its previous system.
//!
//! Files, compression and the USB wait are the edge's ([`BackupEdge`]).

use crate::facts::Snapshot;
use crate::fastboot::{FbError, Framing, Transport};
use crate::format1::{Description, Mode};
use crate::glassine_read::{self, Disk, DiskRecord, RR_CLASS, RR_PROTOCOL, RR_SUBCLASS, ReadError, Reader, Reboot};
use crate::inspect::{self, Host, InspectError, UsbIds};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::fmt;

/// How long the RAM environment may take to show up on USB after `boot`.
/// Its USB controller binds late on some boards.
pub const RAM_WAIT_SECONDS: u64 = 180;

/// The kind of the mode the RAM environment is.
pub const RAM_MODE_KIND: &str = "glassine-ram";

/// The interface of the RAM environment's reader, as a description writes it.
pub fn ram_interface() -> String {
    format!("{RR_CLASS:02x}-{RR_SUBCLASS:02x}-{RR_PROTOCOL:02x}")
}

/// The RAM environment's USB ids, from its description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RamUsb {
    pub vendor_id: u16,
    pub product_id: u16,
}

impl fmt::Display for RamUsb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04x}:{:04x} ({})", self.vendor_id, self.product_id, ram_interface())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupError {
    /// The inspect before the boot failed (not fresh, no answer, ...).
    Inspect(InspectError),
    /// Refused before anything was sent beyond the inspect's reads.
    Refused(String),
    /// Booting the image changes the slot flags, and that wasn't accepted.
    /// Nothing beyond the inspect's reads was sent.
    SlotFlags(String),
    /// `download` or `boot` failed.
    Boot(FbError),
    /// The RAM environment didn't come up as the description says.
    NoRamEnvironment(String),
    /// Reading, checking or storing a disk failed.
    Read(ReadError),
    /// The edge failed (writing the manifest, ...).
    Edge(String),
}

impl BackupError {
    /// A stable name for records and pages.
    pub fn kind(&self) -> &'static str {
        match self {
            BackupError::Inspect(e) => e.kind(),
            BackupError::Refused(_) => "refused",
            BackupError::SlotFlags(_) => "slot-flags",
            BackupError::Boot(_) => "boot",
            BackupError::NoRamEnvironment(_) => "no-ram-environment",
            BackupError::Read(_) => "read",
            BackupError::Edge(_) => "edge",
        }
    }
}

impl fmt::Display for BackupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BackupError::Inspect(e) => write!(f, "{e}"),
            BackupError::Refused(w) => write!(f, "refused: {w}"),
            BackupError::SlotFlags(w) => write!(f, "{w}"),
            BackupError::Boot(e) => write!(f, "starting the RAM environment failed: {e}"),
            BackupError::NoRamEnvironment(w) => write!(f, "the RAM environment: {w}"),
            BackupError::Read(e) => write!(f, "the backup: {e}"),
            BackupError::Edge(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for BackupError {}

impl From<InspectError> for BackupError {
    fn from(e: InspectError) -> Self {
        BackupError::Inspect(e)
    }
}

impl From<ReadError> for BackupError {
    fn from(e: ReadError) -> Self {
        BackupError::Read(e)
    }
}

/// What the backup needs from the edge: the wait for the RAM environment's USB
/// device, where the copies go, and where they are read back from.
pub trait BackupEdge {
    type Ram: Transport;
    /// Waits up to `seconds` for the one device with `usb`'s ids and an
    /// interface ff-47-52, and opens that interface.
    fn wait_ram(&mut self, usb: &RamUsb, seconds: u64) -> impl Future<Output = Result<(Self::Ram, UsbIds), String>>;
    /// Starts the stored copy of `disk`. It must not exist yet.
    fn begin(&mut self, disk: &Disk) -> Result<(), String>;
    /// The next bytes of the copy begun last.
    fn write(&mut self, data: &[u8]) -> Result<(), String>;
    /// Ends the copy begun last, durably; returns the name it is stored under.
    fn end(&mut self, disk: &Disk) -> Result<String, String>;
    /// Opens the stored copy of `disk` to read it back from the start.
    fn reopen(&mut self, disk: &Disk) -> Result<(), String>;
    /// Fills `buf` from the copy reopened last; 0 at its end.
    fn fill(&mut self, buf: &mut [u8]) -> Result<usize, String>;
    /// Stores the manifest (JSON).
    fn manifest(&mut self, text: &str) -> Result<(), String>;
    /// Bytes of `disk` read so far.
    fn progress(&mut self, disk: &Disk, done: u64);
    /// A line for the person.
    fn note(&mut self, line: &str);
}

#[derive(Debug, Clone, Copy)]
pub struct BackupOptions<'a> {
    /// The RAM environment's boot image.
    pub image: &'a [u8],
    /// Booting may change the slot flags (see [`BackupError::SlotFlags`]).
    pub accept_slot_flags: bool,
}

/// What the checks before the boot found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prepared {
    pub description: String,
    /// The fastboot mode the boot loader is in.
    pub mode: String,
    pub framing: Framing,
    pub ram: RamUsb,
    /// What booting changes on the device, when it changes anything.
    pub slot_flags: Option<String>,
    /// The bytes `download` sends: the image, NUL-padded for the framing.
    pub download_size: u64,
}

#[derive(Debug, Clone)]
pub struct BackupDone {
    pub prepared: Prepared,
    /// One record per disk, with the name its copy is stored under.
    pub disks: Vec<(DiskRecord, String)>,
    pub manifest: String,
    /// Whether the RAM environment answered the final reboot.
    pub rebooted: bool,
}

fn parse_size(v: &str) -> Option<u64> {
    match v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16).ok(),
        None => v.parse().ok(),
    }
}

fn hex_id(s: &str) -> Option<u16> {
    (s.len() == 4).then(|| u16::from_str_radix(s, 16).ok()).flatten()
}

/// The bytes `download` sends for an image of `len` bytes: a whole number of
/// transfers under equal-length framing, the image itself otherwise.
pub fn download_size(len: u64, framing: Framing) -> u64 {
    match framing {
        Framing::Plain => len,
        Framing::EqualLength { len: l } => len.div_ceil(l as u64) * l as u64,
    }
}

/// What booting an image changes on a device whose boot loader keeps A/B slot
/// flags, from the boot loader's source (the `boot` command clears the active
/// slot's unbootable flag and marks the slot active before it starts the
/// image).
pub const SLOT_FLAG_CHANGES: &str = "the boot loader rewrites the slot flags in the partition table (GPT) before it starts the image: \
it clears the active slot's unbootable flag, may lower that slot's boot retry count, marks the active slot's partitions active and \
the other slot's inactive, and writes the primary and the backup GPT. No partition's contents change, and the active slot stays the same";

/// The checks before anything but reads is sent: the description and its
/// RAM environment mode, the image, and whether booting it changes the slot
/// flags.
pub fn prepare(ds: &[&Description], snapshot: &Snapshot, framing: Framing, image: &[u8]) -> Result<Prepared, BackupError> {
    let vars = &snapshot.fastboot.as_ref().ok_or(BackupError::Refused("no fastboot facts".into()))?.variables;
    let var = |k: &str| vars.get(k).cloned().flatten();
    let table = inspect::framing_table(ds)?;
    let (_, holding) = inspect::choose_framing(&table, var("product").as_deref())?;
    let Some(rule) = holding.first() else {
        return Err(BackupError::Refused(
            "no device description knows this boot loader, so nothing says how its RAM environment comes up".into(),
        ));
    };
    if holding.iter().any(|r| r.description != rule.description || r.mode != rule.mode) {
        let who: Vec<String> = holding.iter().map(|r| format!("{} mode {}", r.description, r.mode)).collect();
        return Err(BackupError::Refused(format!("more than one description mode matches: {}", who.join(", "))));
    }
    let d = ds.iter().find(|d| d.id == rule.description).expect("a rule's description");
    let mode: &Mode = d.modes.iter().find(|m| m.id == rule.mode).expect("a rule's mode");
    let never = mode
        .departures
        .iter()
        .find(|x| x.kind == "never-use" && x.params.get("operation").and_then(|v| v.as_str()) == Some("boot-image"));
    // The mode restarts into the RAM environment, or the description says
    // booting an image here is possible but changes the device (never-use):
    // then it is only done when accepted.
    let ram_mode = d
        .modes
        .iter()
        .find(|m| m.kind == RAM_MODE_KIND && (mode.restarts_into.contains(&m.id) || never.is_some()))
        .ok_or_else(|| BackupError::Refused(format!("{} mode {} doesn't restart into a RAM environment", d.id, mode.id)))?;
    let ram = ram_mode
        .usb
        .as_ref()
        .filter(|u| u.interface.as_deref() == Some(ram_interface().as_str()))
        .and_then(|u| {
            Some(RamUsb {
                vendor_id: hex_id(&u.vendor_id)?,
                product_id: hex_id(&u.product_id)?,
            })
        })
        .ok_or_else(|| BackupError::Refused(format!("{} mode {} has no USB ids with the interface {}", d.id, ram_mode.id, ram_interface())))?;

    if image.len() < 8 || &image[..8] != b"ANDROID!" {
        return Err(BackupError::Refused("the RAM environment's image isn't an Android boot image".into()));
    }
    let size = download_size(image.len() as u64, framing);
    if size > u32::MAX as u64 {
        return Err(BackupError::Refused(format!("the image is {size} bytes, too large for one download")));
    }
    match var("max-download-size").as_deref().and_then(parse_size) {
        Some(max) if size <= max => {}
        Some(max) => {
            return Err(BackupError::Refused(format!(
                "the image would be sent as {size} bytes, more than the boot loader takes ({max})"
            )));
        }
        None => return Err(BackupError::Refused("the boot loader didn't say how much it takes (max-download-size)".into())),
    }

    let slots = var("slot-count").as_deref().and_then(parse_size).unwrap_or(0);
    let slot_flags = (slots >= 2 || never.is_some()).then(|| {
        let mut why = Vec::new();
        if slots >= 2 {
            why.push(format!("the boot loader reports {slots} slots"));
        }
        if let Some(n) = never {
            let reason = n.params.get("reason").and_then(|v| v.as_str()).unwrap_or("no reason given");
            why.push(format!("{} mode {} says not to boot an image here ({reason})", d.id, mode.id));
        }
        format!("booting the RAM environment changes the device: {}. Here {SLOT_FLAG_CHANGES}", why.join("; "))
    });
    Ok(Prepared {
        description: d.id.clone(),
        mode: mode.id.clone(),
        framing,
        ram,
        slot_flags,
        download_size: size,
    })
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A disk name is a file name part: letters, digits, `_` and `-`.
fn plain_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 64 && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The manifest: what was read, from which device description, and each
/// copy's name, size and sha256. Never a serial or another unit-unique value.
pub fn manifest(p: &Prepared, image: &[u8], disks: &[(DiskRecord, String)]) -> String {
    let v = serde_json::json!({
        "manifest_format": 1,
        "kind": "glassine-ram-backup",
        "description": p.description,
        "mode": p.mode,
        "ram_image_sha256": hex(&Sha256::digest(image)),
        "verified": "every chunk against the device's sha256, every range hashed again on the device, the stored copy read back",
        "disks": disks.iter().map(|(r, file)| serde_json::json!({
            "name": r.disk.name,
            "size": r.disk.size,
            "sector": r.disk.sector,
            "sha256": r.sha256_hex(),
            "file": file,
        })).collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&v).unwrap() + "\n"
}

/// The whole backup: inspect, boot the RAM environment, read every disk,
/// check, store, reboot. Nothing but the inspect's reads is sent before every
/// check of [`prepare`] passed; nothing is ever sent to write a disk.
pub async fn back_up_device<T: Transport, E: BackupEdge>(
    ds: &[&Description],
    fastboot: T,
    ids: &UsbIds,
    host: &Host,
    opts: BackupOptions<'_>,
    edge: &mut E,
) -> Result<BackupDone, BackupError> {
    let (got, mut s) = inspect::inspect_open(ds, fastboot, ids, host).await?;
    let prepared = prepare(ds, &got.snapshot, got.framing, opts.image)?;
    if let Some(w) = &prepared.slot_flags {
        if !opts.accept_slot_flags {
            return Err(BackupError::SlotFlags(w.clone()));
        }
        edge.note(&format!("accepted: {w}"));
    }
    edge.note(&format!(
        "starting the RAM environment ({} bytes, {})",
        prepared.download_size,
        prepared.framing.name()
    ));
    s.download_and_boot(opts.image).await.map_err(BackupError::Boot)?;
    drop(s);

    edge.note(&format!("waiting up to {RAM_WAIT_SECONDS} s for the RAM environment ({})", prepared.ram));
    let (t, rids) = edge.wait_ram(&prepared.ram, RAM_WAIT_SECONDS).await.map_err(BackupError::NoRamEnvironment)?;
    let triple = (RR_CLASS, RR_SUBCLASS, RR_PROTOCOL);
    if rids.vendor_id != prepared.ram.vendor_id || rids.product_id != prepared.ram.product_id || !rids.interfaces.contains(&triple) {
        return Err(BackupError::NoRamEnvironment(format!(
            "the device that came up is {:04x}:{:04x}, not {}",
            rids.vendor_id, rids.product_id, prepared.ram
        )));
    }
    let mut r = Reader::new(t);
    let result = read_all(&mut r, edge).await;
    // The RAM environment reboots by itself when nothing comes for a while;
    // asking now is quicker. After a failure the answer may not parse.
    let rebooted = r.reboot(Reboot::Normal).await.is_ok();
    let disks = result?;
    let manifest = manifest(&prepared, opts.image, &disks);
    edge.manifest(&manifest).map_err(BackupError::Edge)?;
    Ok(BackupDone {
        prepared,
        disks,
        manifest,
        rebooted,
    })
}

async fn read_all<T: Transport, E: BackupEdge>(r: &mut Reader<T>, edge: &mut E) -> Result<Vec<(DiskRecord, String)>, BackupError> {
    let listing = r.list().await?;
    if listing.disks.is_empty() {
        return Err(BackupError::NoRamEnvironment("it lists no disks".into()));
    }
    let mut out = Vec::new();
    for disk in &listing.disks {
        if !plain_name(&disk.name) || out.iter().any(|(r, _): &(DiskRecord, String)| r.disk.name == disk.name) {
            return Err(BackupError::NoRamEnvironment(format!("a disk name it can't be stored under: {:?}", disk.name)));
        }
        edge.begin(disk).map_err(BackupError::Edge)?;
        let rec = {
            let cell = RefCell::new(&mut *edge);
            glassine_read::back_up(r, disk.index, |d| cell.borrow_mut().write(d), |done, _| cell.borrow_mut().progress(disk, done)).await?
        };
        let file = edge.end(disk).map_err(BackupError::Edge)?;
        glassine_read::verify_on_device(r, &rec).await?;
        edge.reopen(disk).map_err(BackupError::Edge)?;
        glassine_read::check_copy(&rec, |b| edge.fill(b))?;
        edge.note(&format!("{}: {} bytes, sha256 {}, checked", disk.name, disk.size, rec.sha256_hex()));
        out.push((rec, file));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(download_size(10, Framing::Plain), 10);
        assert_eq!(download_size(65535, Framing::EqualLength { len: 65535 }), 65535);
        assert_eq!(download_size(65536, Framing::EqualLength { len: 65535 }), 131070);
        assert_eq!(parse_size("0x20000000"), Some(536870912));
        assert_eq!(parse_size("536870912"), Some(536870912));
        assert_eq!(parse_size("x"), None);
        assert!(plain_name("mmcblk0boot1"));
        assert!(!plain_name("../x") && !plain_name("") && !plain_name("a b"));
    }
}
