// SPDX-License-Identifier: GPL-2.0-or-later
//! Reading a device's storage in its Qualcomm emergency download mode with
//! the person's own programmer (spec 004): the read route, from the boot
//! ROM's HELLO to the restart.
//!
//! 1. Sahara: the boot ROM's HELLO, its facts in command mode
//!    ([`sahara::emergency_info`]); a chip this installer doesn't know gets
//!    nothing sent.
//! 2. The programmer the person supplied is sent ([`Sahara::upload`]). The
//!    installer never builds or sends a program of its own (FR-005): the
//!    bytes come from the caller, who got them from the person (FR-003).
//! 3. Firehose: `configure`, and the programmer's first answer (FR-006);
//!    until it answered nothing is read.
//! 4. `getstorageinfo` (when the programmer has it), the sector size, the
//!    GPT, read twice.
//! 5. The images [`Scope`] asks for, each read twice range by range and
//!    compared, then the stored copy read back (FR-008); a mismatch makes the
//!    image incomplete.
//! 6. `power` reset, also after a failure, whenever the programmer answered.
//!
//! Only list, read and restart reach the device (FR-007): the Firehose
//! client has no other request ([`crate::firehose`]).
//!
//! The images and the manifest follow the RAM route's backup
//! ([`crate::ram_backup`]): one stored copy per image, checked with the same
//! [`DiskRecord`] and [`glassine_read::check_copy`], and a JSON manifest
//! beside them. What differs: the images can be single partitions, a
//! manifest names the programmer's checksum instead of the RAM image's, and
//! incomplete images are listed in it.

use crate::fastboot::Transport;
use crate::firehose::{self, Configured, EMMC_SECTOR, Firehose, FirehoseError, StorageInfo};
use crate::glassine_read::{self, Disk, DiskRecord};
use crate::sahara::{self, DoneStatus, EmergencyFacts, Sahara, SaharaError};
use crate::table::{self, ReadAt, Table};
use sha2::{Digest, Sha256};
use std::fmt;

/// The physical partition of the eMMC's user area, where the GPT and every
/// partition the installation writes are. The boot areas (1, 2) and RPMB are
/// never read here.
pub const USER_AREA: u32 = 0;

/// The name the user area's whole image is stored under.
pub const USER_AREA_NAME: &str = "emmc-user";

/// The sectors read for the GPT: the protective MBR, the header and 128
/// entries of 128 bytes. A table with more entries is read further.
pub const GPT_SECTORS: u64 = 34;
/// The most read for the GPT's entries.
const GPT_MAX: u64 = 1 << 20;

/// How much is read before the same range is read again and compared.
pub const RANGE: u64 = 64 << 20;

/// What to read after the GPT.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Nothing more: the partition table is printed, nothing stored.
    GptOnly,
    /// These partitions of the GPT, by name, each into its own image.
    Partitions(Vec<String>),
    /// The whole user area into one image.
    WholeDisk,
}

/// Sizes of the reads; the defaults are [`firehose::READ_CHUNK`] and
/// [`RANGE`]. Tests use small ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sizes {
    pub chunk: u64,
    pub range: u64,
}

impl Default for Sizes {
    fn default() -> Self {
        Sizes {
            chunk: firehose::READ_CHUNK,
            range: RANGE,
        }
    }
}

/// Where the images go: the RAM backup's storing steps without its USB wait.
pub trait ImageEdge {
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
    /// Marks the copy of `disk` incomplete (ending it first if it was being
    /// written), so it is never taken for a backup; returns its new name.
    fn incomplete(&mut self, disk: &Disk) -> Result<String, String>;
    /// Stores the manifest (JSON).
    fn manifest(&mut self, text: &str) -> Result<(), String>;
    /// Bytes of `disk` read so far (first pass).
    fn progress(&mut self, disk: &Disk, done: u64);
    /// A line for the person.
    fn note(&mut self, line: &str);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmergencyError {
    /// Waiting for the boot ROM, reading its facts.
    BootRom(SaharaError),
    /// A chip this installer doesn't know: nothing was sent.
    UnknownChip(String),
    /// The boot ROM refused the programmer, or didn't start it.
    Refused(String),
    /// The programmer didn't answer its first request (FR-006).
    NotWorking(String),
    /// The programmer answered, then something failed.
    Firehose(FirehoseError),
    /// The storage isn't what this route reads (block size, no GPT, ...).
    Storage(String),
    /// A partition asked for isn't in the GPT, or has a name it can't be
    /// stored under.
    Partition(String),
    /// An image read differently twice, or its stored copy differs: it is
    /// marked incomplete.
    Mismatch { image: String, offset: u64 },
    /// Storing failed.
    Edge(String),
}

impl fmt::Display for EmergencyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EmergencyError::BootRom(e) => write!(f, "the boot ROM: {e}"),
            EmergencyError::UnknownChip(w) => write!(f, "{w}"),
            EmergencyError::Refused(w) => write!(f, "the programmer isn't working on this device: {w}"),
            EmergencyError::NotWorking(w) => write!(f, "the programmer isn't working on this device: {w}"),
            EmergencyError::Firehose(e) => write!(f, "{e}"),
            EmergencyError::Storage(w) => write!(f, "the storage: {w}"),
            EmergencyError::Partition(w) => write!(f, "{w}"),
            EmergencyError::Mismatch { image, offset } => {
                write!(
                    f,
                    "{image}: the range at byte {offset} didn't read the same twice (or its stored copy differs); the image is incomplete"
                )
            }
            EmergencyError::Edge(e) => write!(f, "storing the images failed: {e}"),
        }
    }
}

impl std::error::Error for EmergencyError {}

impl From<FirehoseError> for EmergencyError {
    fn from(e: FirehoseError) -> Self {
        EmergencyError::Firehose(e)
    }
}

/// Whether the device was sent a restart at the end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Restart {
    /// The programmer (or the boot ROM) acknowledged it.
    Done,
    /// Sent, without an acknowledgement (the reason).
    Failed(String),
    /// Nothing that could restart it was answering: the person holds the
    /// power key.
    NotPossible,
}

/// A failed run: what failed, and whether the device was restarted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub error: EmergencyError,
    pub restart: Restart,
    /// The manifest, when images were stored before the failure.
    pub manifest: Option<String>,
}

/// One image of the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// Its name, its physical partition (as `index`), size and sector size.
    pub disk: Disk,
    pub start_sector: u64,
}

#[derive(Debug, Clone)]
pub struct Done {
    pub chip: Option<String>,
    pub oem_pk_hash: String,
    pub configured: Configured,
    pub storage: Option<StorageInfo>,
    pub sector: u32,
    pub disk_size: u64,
    pub table: Table,
    /// Each stored image, with its start sector and the name it is stored under.
    pub images: Vec<(DiskRecord, u64, String)>,
    pub manifest: Option<String>,
    pub restart: Restart,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A name an image can be stored under: letters, digits, `_` and `-`.
pub fn storable_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 64 && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// The first bytes of a disk, read into memory, for the GPT parser: reads
/// beyond them fail.
struct Head<'a>(&'a [u8]);

impl ReadAt for Head<'_> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let mut s: &[u8] = self.0;
        s.read_at(offset, buf).map_err(|_| "outside the part of the disk read for the GPT".to_string())
    }
}

/// The GPT of the head of a disk, as [`table::read`] parses it, and the
/// header's numbers this route needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gpt {
    /// The backup header's LBA: the disk's last sector.
    pub alternate_lba: u64,
    /// How many bytes from the start hold the header and the entries.
    pub head_len: u64,
}

/// The primary GPT header at LBA 1 of a disk with `sector`-byte sectors:
/// `None` when there is no `EFI PART` signature there.
pub fn gpt_header(head: &[u8], sector: u32) -> Option<Gpt> {
    let s = sector as usize;
    let h = head.get(s..s + 92)?;
    if &h[..8] != b"EFI PART" {
        return None;
    }
    let (elba, n, esize) = (table::le64(h, 72), table::le32(h, 80) as u64, table::le32(h, 84) as u64);
    let end = elba.checked_mul(sector as u64)?.checked_add(n.checked_mul(esize)?)?;
    Some(Gpt {
        alternate_lba: table::le64(h, 32),
        head_len: end.div_ceil(sector as u64) * sector as u64,
    })
}

/// One line per partition: number, name, start sector, size. No GUIDs (the
/// unique ones name the unit).
pub fn render_table(t: &Table, sector: u32) -> String {
    let mut o = String::new();
    for e in &t.entries {
        o += &format!(
            "  {:>3}  {:<24} start {:>10}  {:>10} sectors  {:>8} KiB\n",
            e.index,
            e.name.as_deref().unwrap_or(""),
            e.start / sector as u64,
            e.size / sector as u64,
            e.size >> 10
        );
    }
    o
}

/// The manifest of the images read: the RAM backup's shape, for this route.
/// Never a serial or another unit-unique value.
pub fn manifest(chip: Option<&str>, programmer: &[u8], images: &[(DiskRecord, u64, String)], incomplete: &[(String, String, String)]) -> String {
    let v = serde_json::json!({
        "manifest_format": 1,
        "kind": "glassine-emergency-backup",
        "route": "qualcomm-firehose",
        "chip": chip,
        "programmer_sha256": hex(&Sha256::digest(programmer)),
        "verified": "every range read twice and compared, the stored copy read back",
        "disks": images.iter().map(|(r, start, file)| serde_json::json!({
            "name": r.disk.name,
            "physical_partition": r.disk.index,
            "start_sector": start,
            "size": r.disk.size,
            "sector": r.disk.sector,
            "sha256": r.sha256_hex(),
            "file": file,
        })).collect::<Vec<_>>(),
        "incomplete": incomplete.iter().map(|(name, file, why)| serde_json::json!({
            "name": name,
            "file": file,
            "why": why,
        })).collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&v).unwrap() + "\n"
}

/// Reads `img` into the edge: range by range, each range read twice (the
/// first pass stored and hashed, the second hashed and compared), then the
/// stored copy read back and compared.
pub async fn read_image<T: Transport, E: ImageEdge>(fh: &mut Firehose<T>, img: &Image, sizes: Sizes, edge: &mut E) -> Result<DiskRecord, EmergencyError> {
    let d = &img.disk;
    let s = d.sector as u64;
    if sizes.range == 0 || !sizes.range.is_multiple_of(s) || !d.size.is_multiple_of(s) || d.size == 0 {
        return Err(EmergencyError::Storage(format!("{}: {} bytes in ranges of {}", d.name, d.size, sizes.range)));
    }
    let mut whole = Sha256::new();
    let mut ranges = Vec::new();
    let mut off = 0u64;
    while off < d.size {
        let len = sizes.range.min(d.size - off);
        let start = img.start_sector + off / s;
        let mut first = Sha256::new();
        let mut err = None;
        fh.read(d.index, d.sector, start, len / s, sizes.chunk, &mut |b| {
            first.update(b);
            whole.update(b);
            edge.write(b).inspect_err(|e| err = Some(e.clone()))
        })
        .await
        .map_err(|e| match err.take() {
            Some(w) => EmergencyError::Edge(w),
            None => e.into(),
        })?;
        let mut second = Sha256::new();
        fh.read(d.index, d.sector, start, len / s, sizes.chunk, &mut |b| {
            second.update(b);
            Ok(())
        })
        .await?;
        let h: [u8; 32] = first.finalize().into();
        if h != <[u8; 32]>::from(second.finalize()) {
            return Err(EmergencyError::Mismatch {
                image: d.name.clone(),
                offset: off,
            });
        }
        ranges.push(h);
        off += len;
        edge.progress(d, off);
    }
    Ok(DiskRecord {
        disk: d.clone(),
        sha256: whole.finalize().into(),
        range: sizes.range,
        ranges,
    })
}

/// Reads, stores and checks one image; a failure after it was begun marks it
/// incomplete.
async fn store_image<T: Transport, E: ImageEdge>(
    fh: &mut Firehose<T>,
    img: &Image,
    sizes: Sizes,
    edge: &mut E,
    incomplete: &mut Vec<(String, String, String)>,
) -> Result<(DiskRecord, String), EmergencyError> {
    edge.begin(&img.disk).map_err(EmergencyError::Edge)?;
    let r = async {
        let rec = read_image(fh, img, sizes, edge).await?;
        let file = edge.end(&img.disk).map_err(EmergencyError::Edge)?;
        edge.reopen(&img.disk).map_err(EmergencyError::Edge)?;
        glassine_read::check_copy(&rec, |b| edge.fill(b)).map_err(|e| match e {
            glassine_read::ReadError::CopyDiffers { disk, offset } => EmergencyError::Mismatch { image: disk, offset },
            e => EmergencyError::Edge(e.to_string()),
        })?;
        Ok::<_, EmergencyError>((rec, file))
    }
    .await;
    if let Err(e) = &r {
        let file = edge.incomplete(&img.disk).unwrap_or_else(|w| format!("(not marked: {w})"));
        incomplete.push((img.disk.name.clone(), file, e.to_string()));
    }
    r
}

/// The run against a device at its boot ROM's first HELLO. `programmer` is
/// the person's file; `scope` what to read after the GPT.
pub async fn run<T: Transport, E: ImageEdge>(t: T, programmer: &[u8], scope: &Scope, sizes: Sizes, edge: &mut E) -> Result<Done, Failure> {
    let fail = |error, restart| Failure {
        error,
        restart,
        manifest: None,
    };
    let mut s = Sahara::connect(t).await.map_err(|e| fail(EmergencyError::BootRom(e), Restart::NotPossible))?;
    let facts: EmergencyFacts = sahara::emergency_info(&mut s)
        .await
        .map_err(|e| fail(EmergencyError::BootRom(e), Restart::NotPossible))?;
    let chip = facts.chip().map(str::to_string);
    let Some(chip_name) = chip.clone() else {
        return Err(fail(
            EmergencyError::UnknownChip(format!(
                "the boot ROM reports msm id {:08x}, a chip this installer doesn't know; nothing was sent",
                facts.hw().msm_id
            )),
            Restart::NotPossible,
        ));
    };
    edge.note(&format!("boot ROM: {chip_name}, oem pk hash {}", facts.pk_hash_hex()));
    edge.note(&format!("sending the programmer ({} bytes)", programmer.len()));
    let refused = match s.upload(programmer).await {
        Ok(up) if up.done == DoneStatus::Complete => None,
        Ok(up) => Some(format!("the boot ROM ended the transfer without starting it ({:?})", up.done)),
        Err(e) => Some(e.to_string()),
    };
    if let Some(why) = refused {
        let restart = match s.reset().await {
            Ok(()) => Restart::Done,
            Err(e) => Restart::Failed(e.to_string()),
        };
        return Err(fail(EmergencyError::Refused(why), restart));
    }
    let mut fh = Firehose::new(s.into_transport());
    let c = match fh.configure().await {
        Ok(c) => c.clone(),
        Err(FirehoseError::NoAnswer) => {
            return Err(fail(
                EmergencyError::NotWorking("the boot ROM took it, but it didn't answer its first request".into()),
                Restart::NotPossible,
            ));
        }
        Err(e @ (FirehoseError::Nak { .. } | FirehoseError::NeedsAuth { .. })) => {
            // It answers, so it can be asked to restart.
            let restart = match fh.reset().await {
                Ok(()) => Restart::Done,
                Err(e) => Restart::Failed(e.to_string()),
            };
            return Err(fail(EmergencyError::NotWorking(e.to_string()), restart));
        }
        Err(e) => return Err(fail(EmergencyError::NotWorking(e.to_string()), Restart::NotPossible)),
    };
    edge.note(&format!(
        "the programmer answers{}{}",
        c.target_name.as_deref().map(|n| format!(" (target {n})")).unwrap_or_default(),
        if c.renegotiated {
            format!(", payload size {} after it asked for it", c.payload_to_target)
        } else {
            String::new()
        }
    ));
    let mut incomplete = Vec::new();
    let mut images = Vec::new();
    let body = read_all(&mut fh, scope, sizes, edge, &mut images, &mut incomplete).await;
    let restart = match fh.reset().await {
        Ok(()) => Restart::Done,
        Err(e) => Restart::Failed(e.to_string()),
    };
    let manifest = if images.is_empty() && incomplete.is_empty() {
        None
    } else {
        let m = manifest(chip.as_deref(), programmer, &images, &incomplete);
        match edge.manifest(&m) {
            Ok(()) => Some(m),
            Err(e) => {
                return Err(Failure {
                    error: EmergencyError::Edge(e),
                    restart,
                    manifest: None,
                });
            }
        }
    };
    match body {
        Ok((storage, sector, disk_size, table)) => Ok(Done {
            chip,
            oem_pk_hash: facts.pk_hash_hex(),
            configured: c,
            storage,
            sector,
            disk_size,
            table,
            images,
            manifest,
            restart,
        }),
        Err(error) => Err(Failure { error, restart, manifest }),
    }
}

type Read = (Option<StorageInfo>, u32, u64, Table);

async fn read_all<T: Transport, E: ImageEdge>(
    fh: &mut Firehose<T>,
    scope: &Scope,
    sizes: Sizes,
    edge: &mut E,
    images: &mut Vec<(DiskRecord, u64, String)>,
    incomplete: &mut Vec<(String, String, String)>,
) -> Result<Read, EmergencyError> {
    let storage = fh.storage_info(USER_AREA).await?;
    match &storage {
        Some(i) => edge.note(&format!(
            "storage: {} blocks of {} bytes, {} physical partitions, {}",
            i.total_blocks.map_or("?".into(), |n| n.to_string()),
            i.block_size.map_or("?".into(), |n| n.to_string()),
            i.num_physical.map_or("?".into(), |n| n.to_string()),
            i.mem_type.as_deref().unwrap_or("type not said")
        )),
        None => edge.note("storage: the programmer doesn't report it (getstorageinfo); the GPT tells the size"),
    }
    if let Some(m) = storage.as_ref().and_then(|i| i.mem_type.as_deref())
        && !m.eq_ignore_ascii_case(firehose::MEMORY_NAME)
    {
        return Err(EmergencyError::Storage(format!(
            "the programmer reports {m} storage; this route reads eMMC only"
        )));
    }
    let sector = match storage.as_ref().and_then(|i| i.block_size) {
        None | Some(EMMC_SECTOR) => EMMC_SECTOR,
        Some(n) => {
            return Err(EmergencyError::Storage(format!(
                "the programmer reports {n}-byte blocks for eMMC, whose sectors are {EMMC_SECTOR} bytes; nothing more is read"
            )));
        }
    };
    let s = sector as u64;
    // The GPT, read twice.
    let mut head = fh.read_vec(USER_AREA, sector, 0, GPT_SECTORS).await?;
    if fh.read_vec(USER_AREA, sector, 0, GPT_SECTORS).await? != head {
        return Err(EmergencyError::Mismatch {
            image: "GPT".into(),
            offset: 0,
        });
    }
    let gpt =
        gpt_header(&head, sector).ok_or_else(|| EmergencyError::Storage(format!("no GPT header at LBA 1 with {sector}-byte sectors; nothing more is read")))?;
    if gpt.head_len > head.len() as u64 {
        if gpt.head_len > GPT_MAX {
            return Err(EmergencyError::Storage(format!("a GPT whose entries end at byte {}", gpt.head_len)));
        }
        let more = gpt.head_len / s - GPT_SECTORS;
        let a = fh.read_vec(USER_AREA, sector, GPT_SECTORS, more).await?;
        if fh.read_vec(USER_AREA, sector, GPT_SECTORS, more).await? != a {
            return Err(EmergencyError::Mismatch {
                image: "GPT".into(),
                offset: GPT_SECTORS * s,
            });
        }
        head.extend_from_slice(&a);
    }
    let from_gpt = gpt
        .alternate_lba
        .checked_add(1)
        .and_then(|n| n.checked_mul(s))
        .ok_or_else(|| EmergencyError::Storage("a GPT header with an impossible backup LBA".into()))?;
    let disk_size = match storage.as_ref().and_then(|i| i.total_blocks) {
        Some(n) => {
            let size = n.checked_mul(s).ok_or_else(|| EmergencyError::Storage(format!("{n} blocks")))?;
            if size < from_gpt {
                return Err(EmergencyError::Storage(format!(
                    "the programmer reports {size} bytes, the GPT's backup header lies beyond that ({from_gpt})"
                )));
            }
            size
        }
        None => from_gpt,
    };
    let t = table::read(&mut Head(&head), disk_size, s).map_err(EmergencyError::Storage)?;
    if t.kind != table::Kind::Gpt || !t.primary_ok || t.sector != s {
        return Err(EmergencyError::Storage("the primary GPT doesn't check out; nothing more is read".into()));
    }
    edge.note(&format!(
        "GPT: {} partitions, disk {} bytes ({} sectors of {sector})",
        t.entries.len(),
        disk_size,
        disk_size / s
    ));
    edge.note(&render_table(&t, sector));

    let wanted: Vec<Image> = match scope {
        Scope::GptOnly => vec![],
        Scope::WholeDisk => vec![Image {
            disk: Disk {
                index: USER_AREA,
                name: USER_AREA_NAME.into(),
                size: disk_size,
                sector,
            },
            start_sector: 0,
        }],
        Scope::Partitions(names) => {
            let mut v: Vec<Image> = Vec::new();
            for n in names {
                let found: Vec<_> = t.entries.iter().filter(|e| e.name.as_deref() == Some(n.as_str())).collect();
                let e = match found.as_slice() {
                    [e] => *e,
                    [] => return Err(EmergencyError::Partition(format!("no partition named {n:?} in the GPT"))),
                    _ => return Err(EmergencyError::Partition(format!("more than one partition named {n:?} in the GPT"))),
                };
                if !storable_name(n) || n == USER_AREA_NAME || v.iter().any(|i| &i.disk.name == n) {
                    return Err(EmergencyError::Partition(format!("{n:?} can't be stored under its name")));
                }
                v.push(Image {
                    disk: Disk {
                        index: USER_AREA,
                        name: n.clone(),
                        size: e.size,
                        sector,
                    },
                    start_sector: e.start / s,
                });
            }
            v
        }
    };
    for img in &wanted {
        edge.note(&format!(
            "reading {} ({} bytes from sector {}), every range twice",
            img.disk.name, img.disk.size, img.start_sector
        ));
        let (rec, file) = store_image(fh, img, sizes, edge, incomplete).await?;
        edge.note(&format!("{}: {} bytes, sha256 {}, checked", img.disk.name, img.disk.size, rec.sha256_hex()));
        images.push((rec, img.start_sector, file));
    }
    Ok((storage, sector, disk_size, t))
}
