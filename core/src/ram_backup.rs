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
//!    two slots the active slot's flags are read first: booting changes
//!    nothing when the slot is marked successful and not unbootable; any
//!    other state (or flags the boot loader doesn't publish) needs the
//!    caller's acceptance ([`BackupOptions`], [`slot_flag_changes`]).
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

/// The active slot's flags as the boot loader publishes them
/// (`slot-successful:<s>`, `slot-unbootable:<s>`, `slot-retry-count:<s>`),
/// read live before the boot. `None` where it didn't answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlotFlags {
    pub slot: String,
    pub successful: Option<bool>,
    pub unbootable: Option<bool>,
    pub retry_count: Option<u64>,
}

/// The variables that read the flags of `slot`.
pub fn slot_flag_variables(slot: &str) -> [String; 3] {
    [
        format!("slot-successful:{slot}"),
        format!("slot-unbootable:{slot}"),
        format!("slot-retry-count:{slot}"),
    ]
}

fn yes_no(v: Option<&str>) -> Option<bool> {
    match v {
        Some("yes") => Some(true),
        Some("no") => Some(false),
        _ => None,
    }
}

impl SlotFlags {
    /// From the answers to [`slot_flag_variables`], in that order.
    pub fn from_answers(slot: &str, answers: [Option<String>; 3]) -> SlotFlags {
        let [s, u, r] = answers;
        SlotFlags {
            slot: slot.to_string(),
            successful: yes_no(s.as_deref()),
            unbootable: yes_no(u.as_deref()),
            retry_count: r.as_deref().and_then(parse_size),
        }
    }
}

/// What `boot` would change in the slot flags, from the boot loader's source
/// (CmdBoot: ClearUnbootable, then LoadImageAndAuth → FindBootableSlot;
/// UpdatePartitionAttributes writes the GPT only where an attribute differs
/// from the table on the disk). `None`: nothing.
///
/// - A slot marked successful and not unbootable: nothing changes.
/// - Unbootable: CmdBoot clears that bit, a GPT write.
/// - Not successful, retries left: FindBootableSlot lowers the retry count,
///   except for an image from fastboot (CmdBoot sets BootDevImage first), so
///   by the source nothing changes; refused anyway, since that is not tried.
/// - Not successful, no retries left: the slot counts as unbootable and the
///   boot loader falls back to the other slot, rewriting the slot flags.
/// - Flags not published: the change can't be told.
pub fn slot_flag_changes(slot_count: u64, flags: Option<&SlotFlags>) -> Option<String> {
    if slot_count < 2 {
        return None;
    }
    let Some(f) = flags.filter(|f| f.successful.is_some() && f.unbootable.is_some()) else {
        let slot = flags.map(|f| f.slot.as_str()).unwrap_or("?");
        return Some(format!(
            "the boot loader reports {slot_count} slots but doesn't publish slot {slot}'s flags (slot-successful, slot-unbootable), \
so whether booting an image rewrites them in the partition table (GPT) can't be told"
        ));
    };
    let s = &f.slot;
    match (f.successful, f.unbootable, f.retry_count) {
        (Some(true), Some(false), _) => None,
        (_, Some(true), _) => Some(format!(
            "slot {s} is marked unbootable: booting an image clears that bit in the partition table (GPT), both copies{}",
            if f.successful == Some(false) && f.retry_count == Some(0) {
                "; with no retries left the boot loader then falls back to the other slot and rewrites its flags too"
            } else {
                ""
            }
        )),
        (_, _, Some(0)) => Some(format!(
            "slot {s} isn't marked successful and has no retries left: the boot loader treats it as unbootable, \
falls back to the other slot and rewrites the slot flags in the partition table (GPT)"
        )),
        (_, _, n) => Some(format!(
            "slot {s} isn't marked successful (retry count {}): a normal start uses one retry and writes the \
partition table (GPT); by the boot loader's source an image from fastboot skips that, so nothing should change, but that isn't tried",
            n.map_or("not published".to_string(), |n| n.to_string())
        )),
    }
}

/// The checks before anything but reads is sent: the description and its
/// RAM environment mode, the image, and whether booting it changes the slot
/// flags.
pub fn prepare(ds: &[&Description], snapshot: &Snapshot, framing: Framing, image: &[u8], flags: Option<&SlotFlags>) -> Result<Prepared, BackupError> {
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
    if boot_image_departure(mode, "never-use").is_some() {
        let reason = boot_image_departure(mode, "never-use")
            .and_then(|x| x.params.get("reason"))
            .and_then(|v| v.as_str())
            .unwrap_or("no reason given");
        return Err(BackupError::Refused(format!(
            "{} mode {} says never to boot an image here ({reason})",
            d.id, mode.id
        )));
    }
    let check = boot_image_departure(mode, "check-slot-flags");
    // The mode restarts into the RAM environment, or its description allows
    // booting an image after the slot-flag check.
    let ram_mode = d
        .modes
        .iter()
        .find(|m| m.kind == RAM_MODE_KIND && (mode.restarts_into.contains(&m.id) || check.is_some()))
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

    let slots = slot_count(snapshot);
    // A description that asks for the slot-flag check (check-slot-flags) names the
    // hazard; the flags read live tell whether it applies.
    let slots = if check.is_some() { slots.max(2) } else { slots };
    let slot_flags = slot_flag_changes(slots, flags).map(|w| {
        let mut m = format!("booting the RAM environment changes the device: {w}");
        if let Some(n) = check {
            let reason = n.params.get("reason").and_then(|v| v.as_str()).unwrap_or("no reason given");
            m += &format!(" ({} mode {} asks for this check: {reason})", d.id, mode.id);
        }
        m
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

fn snapshot_var(snapshot: &Snapshot, k: &str) -> Option<String> {
    snapshot.fastboot.as_ref()?.variables.get(k).cloned().flatten()
}

fn slot_count(snapshot: &Snapshot) -> u64 {
    snapshot_var(snapshot, "slot-count").as_deref().and_then(parse_size).unwrap_or(0)
}

/// The active slot without its underscore (`a`), as the flag variables name it.
fn current_slot(snapshot: &Snapshot) -> Option<String> {
    let s = snapshot_var(snapshot, "current-slot")?;
    let s = s.trim_start_matches('_');
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric())).then(|| s.to_string())
}

/// The mode's departure of `kind` about `boot-image`, if it has one.
fn boot_image_departure<'a>(mode: &'a Mode, kind: &str) -> Option<&'a crate::format1::Departure> {
    mode.departures
        .iter()
        .find(|x| x.kind == kind && x.params.get("operation").and_then(|v| v.as_str()) == Some("boot-image"))
}

/// Whether the mode the boot loader is in asks for the slot-flag check
/// before an image is booted (`check-slot-flags` on `boot-image`).
fn asks_slot_check(ds: &[&Description], snapshot: &Snapshot) -> bool {
    let Ok(table) = inspect::framing_table(ds) else { return false };
    let Ok((_, holding)) = inspect::choose_framing(&table, snapshot_var(snapshot, "product").as_deref()) else {
        return false;
    };
    holding.iter().any(|r| {
        ds.iter()
            .filter(|d| d.id == r.description)
            .flat_map(|d| d.modes.iter().filter(|m| m.id == r.mode))
            .any(|m| boot_image_departure(m, "check-slot-flags").is_some())
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
    // The active slot's flags, read now: what `boot` would change depends on them.
    let flags = match current_slot(&got.snapshot) {
        Some(slot) if slot_count(&got.snapshot) >= 2 || asks_slot_check(ds, &got.snapshot) => {
            let mut answers: [Option<String>; 3] = Default::default();
            for (a, v) in answers.iter_mut().zip(slot_flag_variables(&slot)) {
                *a = s.getvar(&v).await.map_err(InspectError::from)?;
            }
            s.check_product("after the slot flags").await.map_err(InspectError::from)?;
            Some(SlotFlags::from_answers(&slot, answers))
        }
        _ => None,
    };
    if let Some(f) = &flags {
        edge.note(&format!(
            "slot {}: successful {:?}, unbootable {:?}, retry count {:?}",
            f.slot, f.successful, f.unbootable, f.retry_count
        ));
    }
    let prepared = prepare(ds, &got.snapshot, got.framing, opts.image, flags.as_ref())?;
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
