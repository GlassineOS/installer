// SPDX-License-Identifier: GPL-2.0-or-later
//! Reading a device's storage in its Qualcomm emergency download mode with
//! the person's own programmer (spec 004): the read route, from the boot
//! ROM's HELLO to the restart.
//!
//! 1. Sahara: the boot ROM's HELLO, its facts in command mode
//!    ([`sahara::emergency_info`]); a chip this installer doesn't know gets
//!    nothing sent, and neither does a device whose chip or key hash isn't
//!    the description's ([`run_checked`], [`supplied::before_sending`]).
//! 2. The programmer the person supplied is sent ([`Sahara::upload`]). The
//!    installer never builds or sends a program of its own (FR-005): the
//!    bytes come from the caller, who got them from the person (FR-003).
//! 3. Firehose: `configure`, and the programmer's first answer (FR-006);
//!    until it answered nothing is read.
//! 4. `getstorageinfo` (when the programmer has it), the sector size, the
//!    GPT, read twice. A damaged primary GPT is replaced by the backup at the
//!    disk's last sector; only when both are bad does the run stop. Each
//!    size is noted with where it came from: `getstorageinfo`, a GPT header,
//!    or the single-sector search, whose every read is bounded by
//!    [`PROBE_WAIT`] (a programmer that hangs instead of refusing a read
//!    past the end stops the run, and the device is still restarted).
//! 5. The images [`Scope`] asks for, each read twice range by range and
//!    compared, then the stored copy read back (FR-008); a mismatch makes the
//!    image incomplete. The whole disk (001 FR-009) is the user area and,
//!    when the device has them, the eMMC's two boot areas; never RPMB.
//! 6. `power` reset, also after a failure, whenever the programmer answered.
//!
//! An image is stored under a temporary name and gets its final one only
//! after both reads matched and the stored copy was read back and checked;
//! the manifest is stored last, whole or not at all. A run killed at any
//! point therefore leaves no image under a final name that isn't checked
//! (004 Scenario 5).
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

use crate::fastboot::{FbError, Transport};
use crate::firehose::{self, Configured, EMMC_SECTOR, Firehose, FirehoseError, StorageInfo};
use crate::format1::Description;
use crate::glassine_read::{self, Disk, DiskRecord};
use crate::sahara::{self, DoneStatus, EmergencyFacts, Sahara, SaharaError};
use crate::supplied::{self, Unsendable};
use crate::table::{self, ReadAt, Table};
use sha2::{Digest, Sha256};
use std::fmt;
use std::time::Duration;

/// The physical partition of the eMMC's user area, where the GPT and every
/// partition the installation writes are.
pub const USER_AREA: u32 = 0;

/// The name the user area's whole image is stored under.
pub const USER_AREA_NAME: &str = "emmc-user";

/// The eMMC's boot areas (physical partitions 1 and 2) and the names their
/// images are stored under; read in a whole-disk run when the device has
/// them. RPMB (3) is never read: [`firehose::READABLE_PHYSICAL`] refuses it.
pub const BOOT_AREAS: [(u32, &str); 2] = [(1, "emmc-boot1"), (2, "emmc-boot2")];

/// The largest eMMC boot area: 255 × 128 KiB (JEDEC's `BOOT_SIZE_MULT` is
/// one byte).
pub const BOOT_AREA_MAX: u64 = 255 * (128 << 10);

/// The most sectors the end of the user area is searched up to when nothing
/// reports its size (2^40 sectors: far more than any eMMC).
const PROBE_MAX: u64 = 1 << 40;

/// The bound on each single-sector read of the search for a size: one
/// sector answers at once, so a read that takes longer means the programmer
/// hangs on it instead of refusing it. Applied through
/// [`Transport::set_short_wait`]; a transport without short waits (the
/// browser's) keeps its own bound.
pub const PROBE_WAIT: Duration = Duration::from_secs(5);

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
    /// The whole disk: the user area into one image, then each eMMC boot
    /// area the device has into its own.
    WholeDisk,
    /// Only the eMMC boot areas, read and stored as in a whole-disk run: a
    /// short run that tests that path without reading the user area.
    BootAreas,
    /// Only the eMMC boot areas' sizes, found by the single-sector search
    /// whatever `getstorageinfo` says (it is still asked, and its numbers
    /// noted beside the search's): a read-only run that stores nothing and
    /// shows whether the programmer refuses a read past the end of a
    /// physical partition, or hangs.
    BootAreaSizes,
}

/// How the route reads. The defaults: reads of [`firehose::READ_CHUNK`],
/// checked in ranges of [`RANGE`], no listening before `configure`. Tests use
/// small opts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub chunk: u64,
    pub range: u64,
    /// Read the programmer's own log lines until the transport's first
    /// timeout before `configure` ([`Firehose::listen`]). Only for a
    /// transport that survives a timeout (native USB, not the browser's).
    pub listen_first: bool,
    /// Send `configure` again while it isn't answered, as `qdl` does
    /// ([`Firehose::configure`]). Only for a transport that survives a
    /// timeout.
    pub resend_configure: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            chunk: firehose::READ_CHUNK,
            range: RANGE,
            listen_first: false,
            resend_configure: false,
        }
    }
}

/// Where the images go: the RAM backup's storing steps without its USB wait.
///
/// A copy is written under a temporary name (`begin` to `end`), read back
/// under it, and only then given its final name (`keep`): whatever a killed
/// process leaves behind is never under a final name.
pub trait ImageEdge {
    /// Starts the stored copy of `disk` under a temporary name. Neither that
    /// nor its final name may exist yet.
    fn begin(&mut self, disk: &Disk) -> Result<(), String>;
    /// The next bytes of the copy begun last.
    fn write(&mut self, data: &[u8]) -> Result<(), String>;
    /// Ends the copy begun last, durably, still under its temporary name.
    fn end(&mut self, disk: &Disk) -> Result<(), String>;
    /// Opens the stored copy of `disk` to read it back from the start.
    fn reopen(&mut self, disk: &Disk) -> Result<(), String>;
    /// Fills `buf` from the copy reopened last; 0 at its end.
    fn fill(&mut self, buf: &mut [u8]) -> Result<usize, String>;
    /// Gives the checked copy of `disk` its final name, durably; returns
    /// that name. Called only after both reads matched and the stored copy
    /// was read back and checked.
    fn keep(&mut self, disk: &Disk) -> Result<String, String>;
    /// Marks the copy of `disk` incomplete (ending it first if it was being
    /// written), so it is never taken for a backup; returns its new name.
    fn incomplete(&mut self, disk: &Disk) -> Result<String, String>;
    /// Stores the manifest (JSON) so that it is either there whole or not
    /// at all (a temporary name, then a rename).
    fn manifest(&mut self, text: &str) -> Result<(), String>;
    /// Bytes of `disk` read so far (first pass).
    fn progress(&mut self, disk: &Disk, done: u64);
    /// A line for the person.
    fn note(&mut self, line: &str);
}

/// [`ImageEdge`] for storage that is reached asynchronously (the browser's
/// files): the same steps with the same rules, each storing step awaited.
/// `write` stays synchronous: it is called from inside a read, so an edge
/// that can't write at once queues the bytes and reports a failure at the
/// next `write` or at `end`. Every [`ImageEdge`] is one.
#[allow(async_fn_in_trait)]
pub trait AsyncImageEdge {
    async fn begin(&mut self, disk: &Disk) -> Result<(), String>;
    fn write(&mut self, data: &[u8]) -> Result<(), String>;
    async fn end(&mut self, disk: &Disk) -> Result<(), String>;
    async fn reopen(&mut self, disk: &Disk) -> Result<(), String>;
    async fn fill(&mut self, buf: &mut [u8]) -> Result<usize, String>;
    async fn keep(&mut self, disk: &Disk) -> Result<String, String>;
    async fn incomplete(&mut self, disk: &Disk) -> Result<String, String>;
    async fn manifest(&mut self, text: &str) -> Result<(), String>;
    fn progress(&mut self, disk: &Disk, done: u64);
    fn note(&mut self, line: &str);
}

impl<E: ImageEdge> AsyncImageEdge for E {
    async fn begin(&mut self, disk: &Disk) -> Result<(), String> {
        ImageEdge::begin(self, disk)
    }
    fn write(&mut self, data: &[u8]) -> Result<(), String> {
        ImageEdge::write(self, data)
    }
    async fn end(&mut self, disk: &Disk) -> Result<(), String> {
        ImageEdge::end(self, disk)
    }
    async fn reopen(&mut self, disk: &Disk) -> Result<(), String> {
        ImageEdge::reopen(self, disk)
    }
    async fn fill(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        ImageEdge::fill(self, buf)
    }
    async fn keep(&mut self, disk: &Disk) -> Result<String, String> {
        ImageEdge::keep(self, disk)
    }
    async fn incomplete(&mut self, disk: &Disk) -> Result<String, String> {
        ImageEdge::incomplete(self, disk)
    }
    async fn manifest(&mut self, text: &str) -> Result<(), String> {
        ImageEdge::manifest(self, text)
    }
    fn progress(&mut self, disk: &Disk, done: u64) {
        ImageEdge::progress(self, disk, done)
    }
    fn note(&mut self, line: &str) {
        ImageEdge::note(self, line)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmergencyError {
    /// Waiting for the boot ROM, reading its facts.
    BootRom(SaharaError),
    /// A chip this installer doesn't know: nothing was sent.
    UnknownChip(String),
    /// The device in the mode isn't the described one (its chip or key hash
    /// differs), or the file isn't one to send to it: nothing was sent
    /// ([`run_checked`]).
    NotSent(Unsendable),
    /// A single-sector read of the search for a size got no answer within
    /// [`PROBE_WAIT`]: the programmer hangs on it instead of refusing it.
    /// Nothing more is read.
    ProbeHangs { image: String, physical: u32, lba: u64 },
    /// The boot ROM refused the programmer, or didn't start it.
    Refused(String),
    /// The programmer refused its first request (FR-006): a NAK, or it
    /// wants authentication.
    NotWorking(String),
    /// The boot ROM stopped answering during the upload, or the programmer
    /// didn't answer its first request in time (FR-006: it doesn't count as
    /// working). No verdict on the file: a cable or a slow start looks the
    /// same.
    NoAnswer(String),
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
            EmergencyError::NotSent(u) => write!(f, "{u}"),
            EmergencyError::ProbeHangs { image, physical, lba } => write!(
                f,
                "{image}: the programmer didn't answer a one-sector read at sector {lba} of physical partition {physical} within {} s: it hangs instead of refusing a read it can't do, so its size can't be found by searching; nothing more is read",
                PROBE_WAIT.as_secs()
            ),
            EmergencyError::Refused(w) => write!(f, "the programmer isn't working on this device: {w}"),
            EmergencyError::NotWorking(w) => write!(f, "the programmer isn't working on this device: {w}"),
            EmergencyError::NoAnswer(w) => write!(f, "the programmer isn't working on this device, or the connection was lost: {w}"),
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
    /// The size of each area the run found, and where it came from.
    pub sizes: Vec<AreaSize>,
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
/// `skipped` lists what a whole-disk run didn't read and why (a boot area
/// the device doesn't have).
pub fn manifest(
    chip: Option<&str>,
    programmer: &[u8],
    images: &[(DiskRecord, u64, String)],
    incomplete: &[(String, String, String)],
    skipped: &[Skipped],
) -> String {
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
        "not_read": skipped.iter().map(|k| serde_json::json!({
            "name": k.name,
            "physical_partition": k.physical,
            "why": k.why,
        })).collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&v).unwrap() + "\n"
}

/// A part of the whole disk that wasn't read, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub name: String,
    pub physical: u32,
    pub why: String,
}

/// Reads `img` into the edge: range by range, each range read twice (the
/// first pass stored and hashed, the second hashed and compared), then the
/// stored copy read back and compared.
pub async fn read_image<T: Transport, E: AsyncImageEdge>(fh: &mut Firehose<T>, img: &Image, opts: Options, edge: &mut E) -> Result<DiskRecord, EmergencyError> {
    let d = &img.disk;
    let s = d.sector as u64;
    if opts.range == 0 || !opts.range.is_multiple_of(s) || !d.size.is_multiple_of(s) || d.size == 0 {
        return Err(EmergencyError::Storage(format!("{}: {} bytes in ranges of {}", d.name, d.size, opts.range)));
    }
    let mut whole = Sha256::new();
    let mut ranges = Vec::new();
    let mut off = 0u64;
    while off < d.size {
        let len = opts.range.min(d.size - off);
        let start = img.start_sector + off / s;
        let mut first = Sha256::new();
        let mut err = None;
        fh.read(d.index, d.sector, start, len / s, opts.chunk, &mut |b| {
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
        fh.read(d.index, d.sector, start, len / s, opts.chunk, &mut |b| {
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
        range: opts.range,
        ranges,
    })
}

/// Reads, stores and checks one image; a failure after it was begun marks it
/// incomplete.
async fn store_image<T: Transport, E: AsyncImageEdge>(
    fh: &mut Firehose<T>,
    img: &Image,
    opts: Options,
    edge: &mut E,
    incomplete: &mut Vec<(String, String, String)>,
) -> Result<(DiskRecord, String), EmergencyError> {
    edge.begin(&img.disk).await.map_err(EmergencyError::Edge)?;
    let r = async {
        let rec = read_image(fh, img, opts, edge).await?;
        edge.end(&img.disk).await.map_err(EmergencyError::Edge)?;
        edge.reopen(&img.disk).await.map_err(EmergencyError::Edge)?;
        glassine_read::check_copy_async(&rec, async |b: &mut [u8]| edge.fill(b).await)
            .await
            .map_err(|e| match e {
                glassine_read::ReadError::CopyDiffers { disk, offset } => EmergencyError::Mismatch { image: disk, offset },
                e => EmergencyError::Edge(e.to_string()),
            })?;
        // Both reads matched and the stored copy checked: only now its final name.
        let file = edge.keep(&img.disk).await.map_err(EmergencyError::Edge)?;
        Ok::<_, EmergencyError>((rec, file))
    }
    .await;
    if let Err(e) = &r {
        let file = edge.incomplete(&img.disk).await.unwrap_or_else(|w| format!("(not marked: {w})"));
        incomplete.push((img.disk.name.clone(), file, e.to_string()));
    }
    r
}

/// The device a run is for: its description and the id of the supplied file
/// the programmer is.
#[derive(Clone, Copy, Debug)]
pub struct Expected<'a> {
    pub description: &'a Description,
    pub file: &'a str,
}

/// The run against a device at its boot ROM's first HELLO, for the device
/// `expected` describes. `programmer` is the person's file, checked
/// ([`supplied::check`]) before the device was opened; `scope` what to read
/// after the GPT. Before anything is sent, the chip and key hash the boot ROM
/// reports are compared with the description ([`supplied::before_sending`],
/// 004 FR-004): a device that isn't the described one gets nothing.
pub async fn run_checked<T: Transport, E: AsyncImageEdge>(
    t: T,
    programmer: &[u8],
    expected: Expected<'_>,
    scope: &Scope,
    opts: Options,
    edge: &mut E,
) -> Result<Done, Failure> {
    run_for(t, programmer, Some(expected), scope, opts, edge).await
}

/// [`run_checked`] without a description: only a chip this installer doesn't
/// know is refused; the device's report isn't compared with anything. A
/// route that has the description (the command line, the browser) uses
/// [`run_checked`].
pub async fn run<T: Transport, E: AsyncImageEdge>(t: T, programmer: &[u8], scope: &Scope, opts: Options, edge: &mut E) -> Result<Done, Failure> {
    run_for(t, programmer, None, scope, opts, edge).await
}

async fn run_for<T: Transport, E: AsyncImageEdge>(
    t: T,
    programmer: &[u8],
    expected: Option<Expected<'_>>,
    scope: &Scope,
    opts: Options,
    edge: &mut E,
) -> Result<Done, Failure> {
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
    if let Some(x) = expected {
        // The device's own report against the description, before the upload
        // (004 FR-004): another device gets nothing, not even a reset.
        if let Err(u) = supplied::before_sending(x.description, x.file, programmer, &supplied::emergency_values(&facts)) {
            return Err(fail(EmergencyError::NotSent(u), Restart::NotPossible));
        }
        edge.note(&format!("the boot ROM's chip and key hash are {}'s", x.description.name));
    }
    edge.note(&format!("sending the programmer ({} bytes)", programmer.len()));
    let refused = match s.upload(programmer).await {
        Ok(up) if up.done == DoneStatus::Complete => None,
        Ok(up) => Some(EmergencyError::Refused(format!(
            "the boot ROM ended the transfer without starting it ({:?})",
            up.done
        ))),
        // Silence or a lost connection is no answer about the file.
        Err(e @ SaharaError::Transport(_)) => Some(EmergencyError::NoAnswer(format!("the boot ROM stopped answering during the upload: {e}"))),
        Err(e) => Some(EmergencyError::Refused(e.to_string())),
    };
    if let Some(error) = refused {
        let restart = match s.reset().await {
            Ok(()) => Restart::Done,
            Err(e) => Restart::Failed(e.to_string()),
        };
        return Err(fail(error, restart));
    }
    let mut fh = Firehose::new(s.into_transport());
    if opts.listen_first {
        match fh.listen().await {
            Ok(n) => edge.note(&format!("the programmer started and logged {n} line(s)")),
            Err(e) => return Err(fail(EmergencyError::NoAnswer(e.to_string()), Restart::NotPossible)),
        }
    }
    let c = match fh.configure(opts.resend_configure).await {
        Ok(c) => c.clone(),
        Err(FirehoseError::NoAnswer) => {
            return Err(fail(
                EmergencyError::NoAnswer("the boot ROM took it, but it didn't answer its first request".into()),
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
        Err(e) => return Err(fail(EmergencyError::NoAnswer(e.to_string()), Restart::NotPossible)),
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
    let mut found = Found::default();
    let body = read_all(&mut fh, scope, opts, edge, &mut found).await;
    let Found {
        images,
        incomplete,
        skipped,
        sizes,
    } = found;
    let restart = match fh.reset().await {
        Ok(()) => Restart::Done,
        Err(e) => Restart::Failed(e.to_string()),
    };
    let manifest = if images.is_empty() && incomplete.is_empty() {
        None
    } else {
        let m = manifest(chip.as_deref(), programmer, &images, &incomplete, &skipped);
        match edge.manifest(&m).await {
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
            sizes,
            images,
            manifest,
            restart,
        }),
        Err(error) => Err(Failure { error, restart, manifest }),
    }
}

/// The size of one area of the storage the run found, and where it came from
/// (`getstorageinfo`, a GPT header, or the single-sector search).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreaSize {
    pub name: String,
    pub physical: u32,
    pub bytes: u64,
    pub source: String,
}

type Read = (Option<StorageInfo>, u32, u64, Table);

/// One sector of a physical partition, read once, the wait bounded by
/// [`PROBE_WAIT`]: `None` when the programmer refuses the read (past the
/// end, or an area the device doesn't have). A refusal leaves the session in
/// step; no answer in time is [`EmergencyError::ProbeHangs`] (after which
/// nothing more is read), any other failure ends the run too.
async fn sector_at<T: Transport>(fh: &mut Firehose<T>, image: &str, physical: u32, sector: u32, lba: u64) -> Result<Option<Vec<u8>>, EmergencyError> {
    fh.set_short_wait(Some(PROBE_WAIT));
    let r = fh.read_vec(physical, sector, lba, 1).await;
    fh.set_short_wait(None);
    match r {
        Ok(v) => Ok(Some(v)),
        Err(FirehoseError::Nak { .. }) => Ok(None),
        Err(FirehoseError::Transport(FbError::Timeout(_)) | FirehoseError::Short { .. }) => Err(EmergencyError::ProbeHangs {
            image: image.into(),
            physical,
            lba,
        }),
        Err(e) => Err(e.into()),
    }
}

/// `sectors` sectors from `start`, read twice and compared; `what` names
/// them in a mismatch.
async fn read_twice<T: Transport>(fh: &mut Firehose<T>, physical: u32, sector: u32, start: u64, sectors: u64, what: &str) -> Result<Vec<u8>, EmergencyError> {
    let a = fh.read_vec(physical, sector, start, sectors).await?;
    if fh.read_vec(physical, sector, start, sectors).await? != a {
        return Err(EmergencyError::Mismatch {
            image: what.into(),
            offset: start * sector as u64,
        });
    }
    Ok(a)
}

/// What the single-sector search found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Search {
    /// How many sectors read; `None` when even the sector at the bound read
    /// (the programmer doesn't refuse reads past the end).
    sectors: Option<u64>,
    reads: u32,
    /// The reads the programmer refused (NAK).
    refused: u32,
}

impl Search {
    /// Whether sector `lba` reads, counted.
    async fn at<T: Transport>(&mut self, fh: &mut Firehose<T>, image: &str, physical: u32, sector: u32, lba: u64) -> Result<bool, EmergencyError> {
        self.reads += 1;
        let ok = sector_at(fh, image, physical, sector, lba).await?.is_some();
        if !ok {
            self.refused += 1;
        }
        Ok(ok)
    }

    fn said(&self) -> String {
        format!(
            "the single-sector search ({} one-sector reads, {} of them refused by the programmer past the end)",
            self.reads, self.refused
        )
    }
}

/// How many sectors of `physical` can be read, found by reading single
/// sectors (the programmer refuses those past the end): doubling, then
/// halving. Sector 0 must be readable; at most `max`, and the sector at
/// `max` is read too when all below it read, so that a programmer that
/// never refuses isn't taken for an area of exactly `max` sectors.
async fn probe_end<T: Transport>(fh: &mut Firehose<T>, image: &str, physical: u32, sector: u32, max: u64) -> Result<Search, EmergencyError> {
    let mut s = Search {
        sectors: None,
        reads: 0,
        refused: 0,
    };
    let (mut lo, mut hi) = (0u64, 1u64);
    loop {
        if hi >= max {
            hi = max;
            break;
        }
        if !s.at(fh, image, physical, sector, hi).await? {
            break;
        }
        lo = hi;
        hi = hi.saturating_mul(2);
    }
    // lo reads; hi doesn't, or is the bound.
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if s.at(fh, image, physical, sector, mid).await? {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    s.sectors = Some(lo + 1);
    if lo + 1 == max && s.at(fh, image, physical, sector, max).await? {
        s.sectors = None;
    }
    Ok(s)
}

/// The head of the disk blanked and its last sectors as read: what
/// [`table::read`] is given for the backup GPT, so that the damaged primary
/// is never taken.
struct Tail<'a> {
    blank: u64,
    at: u64,
    tail: &'a [u8],
}

impl ReadAt for Tail<'_> {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let end = offset.checked_add(buf.len() as u64).ok_or("past the end")?;
        if end <= self.blank {
            buf.fill(0);
            return Ok(());
        }
        if offset >= self.at {
            let mut s: &[u8] = self.tail;
            return s.read_at(offset - self.at, buf);
        }
        Err("outside the parts of the disk read for the GPT".into())
    }
}

/// The GPT and the disk's size, and where the size came from: the primary
/// at LBA 1 when it checks out, else the backup at the last LBA.
async fn read_gpt<T: Transport, E: AsyncImageEdge>(
    fh: &mut Firehose<T>,
    sector: u32,
    storage: Option<&StorageInfo>,
    edge: &mut E,
) -> Result<(Table, u64, String), EmergencyError> {
    let s = sector as u64;
    let reported = match storage.and_then(|i| i.total_blocks) {
        Some(n) => Some(n.checked_mul(s).ok_or_else(|| EmergencyError::Storage(format!("{n} blocks")))?),
        None => None,
    };
    let mut head = read_twice(fh, USER_AREA, sector, 0, GPT_SECTORS, "GPT").await?;
    let header = gpt_header(&head, sector);
    let why = match &header {
        None => format!("no GPT header at LBA 1 with {sector}-byte sectors"),
        Some(g) if g.head_len > GPT_MAX => format!("the GPT header at LBA 1 has entries ending at byte {}", g.head_len),
        Some(g) => {
            if g.head_len > head.len() as u64 {
                let more = g.head_len / s - GPT_SECTORS;
                head.extend_from_slice(&read_twice(fh, USER_AREA, sector, GPT_SECTORS, more, "GPT").await?);
            }
            let from_gpt = g.alternate_lba.checked_add(1).and_then(|n| n.checked_mul(s));
            let size = reported.or(from_gpt);
            match size.map(|z| table::read(&mut Head(&head), z, s)) {
                Some(Ok(t)) if t.kind == table::Kind::Gpt && t.primary_ok && t.sector == s => {
                    let size = size.unwrap_or_default();
                    if let (Some(r), Some(f)) = (reported, from_gpt)
                        && r < f
                    {
                        return Err(EmergencyError::Storage(format!(
                            "the programmer reports {r} bytes, the GPT's backup header lies beyond that ({f})"
                        )));
                    }
                    let source = match (reported, from_gpt) {
                        (Some(_), Some(f)) => format!("getstorageinfo (the GPT's backup header at LBA {} lies within it)", f / s - 1),
                        (Some(_), None) => "getstorageinfo".to_string(),
                        (None, _) => format!("the primary GPT header's backup LBA ({})", g.alternate_lba),
                    };
                    return Ok((t, size, source));
                }
                _ => "the primary GPT at LBA 1 doesn't check out".to_string(),
            }
        }
    };
    edge.note(&format!("{why}; looking for the backup GPT at the disk's last sector"));
    // The disk's size: reported, else the damaged header's backup LBA when a
    // GPT header is found there, else searched for.
    let (size, source) = match reported {
        Some(z) => (z, "getstorageinfo".to_string()),
        None => {
            let mut hinted = None;
            if let Some(lba) = header.as_ref().map(|g| g.alternate_lba).filter(|l| *l > GPT_SECTORS && *l < PROBE_MAX)
                && let Some(h) = sector_at(fh, USER_AREA_NAME, USER_AREA, sector, lba).await?
                && &h[..8] == b"EFI PART"
                && table::le64(&h, 24) == lba
            {
                hinted = Some((
                    (lba + 1) * s,
                    format!("the backup GPT header found at the damaged primary's backup LBA ({lba})"),
                ));
            }
            match hinted {
                Some(z) => z,
                None => {
                    edge.note("the disk's size isn't reported: finding its last sector by reading single sectors");
                    let found = probe_end(fh, USER_AREA_NAME, USER_AREA, sector, PROBE_MAX).await?;
                    let n = found.sectors.ok_or_else(|| {
                        EmergencyError::Storage(format!(
                            "the programmer read every sector searched up to {PROBE_MAX}: it doesn't refuse reads past the end, so the disk's size can't be found"
                        ))
                    })?;
                    (n * s, found.said())
                }
            }
        }
    };
    let both_bad = |w: &str| EmergencyError::Storage(format!("{why}, and the backup GPT {w}; nothing more is read"));
    let last = (size / s)
        .checked_sub(1)
        .filter(|l| *l > GPT_SECTORS)
        .ok_or_else(|| both_bad("can't be on a disk this small"))?;
    let bh = read_twice(fh, USER_AREA, sector, last, 1, "backup GPT").await?;
    if &bh[..8] != b"EFI PART" {
        return Err(both_bad(&format!("isn't at LBA {last}")));
    }
    let (elba, n, esize) = (table::le64(&bh, 72), table::le32(&bh, 80) as u64, table::le32(&bh, 84) as u64);
    let len = n
        .checked_mul(esize)
        .filter(|l| *l <= GPT_MAX)
        .ok_or_else(|| both_bad("names entries too large"))?;
    if elba <= GPT_SECTORS || elba >= last || last - elba > len.div_ceil(s) + GPT_SECTORS {
        return Err(both_bad(&format!("names its entries at LBA {elba}")));
    }
    let mut tail = read_twice(fh, USER_AREA, sector, elba, last - elba, "backup GPT").await?;
    tail.extend_from_slice(&bh);
    let mut r = Tail {
        blank: GPT_SECTORS * s,
        at: elba * s,
        tail: &tail,
    };
    match table::read(&mut r, size, s) {
        Ok(t) if t.kind == table::Kind::Gpt && !t.primary_ok && t.sector == s => {
            edge.note(&format!("using the backup GPT at LBA {last}: the primary is damaged"));
            Ok((t, size, source))
        }
        _ => Err(both_bad(&format!("at LBA {last} doesn't check out either"))),
    }
}

/// The size of a boot area and where it came from, or why it isn't read:
/// the programmer's storage facts for it when they are believable and its
/// last sector reads; else found by reading single sectors (which relies on
/// a refusal past the end). With `search`, the facts are only noted beside
/// the search's result, which is always taken (only a partition the
/// programmer names RPMB is still never touched).
async fn boot_area_size<T: Transport>(
    fh: &mut Firehose<T>,
    physical: u32,
    name: &str,
    sector: u32,
    user: Option<&StorageInfo>,
    search: bool,
) -> Result<Result<(u64, String), String>, EmergencyError> {
    let s = sector as u64;
    let info = fh.storage_info(physical).await?;
    if info.as_ref().is_some_and(|i| i.names_rpmb) {
        return Ok(Err("the programmer names it RPMB, which is never read".into()));
    }
    if !search && info.as_ref().and_then(|i| i.total_blocks) == Some(0) {
        return Ok(Err("the programmer reports it empty: the device has none".into()));
    }
    if sector_at(fh, name, physical, sector, 0).await?.is_none() {
        return Ok(Err("the programmer refuses reading it: the device has none, or it isn't readable".into()));
    }
    let blocks = info.as_ref().and_then(|i| i.total_blocks);
    let users = user.and_then(|u| u.total_blocks);
    // Facts equal to the user area's are the user area's, not this one's.
    let reported = blocks.filter(|n| *n > 0 && n.saturating_mul(s) <= BOOT_AREA_MAX && Some(*n) != users);
    if !search
        && let Some(n) = reported
        && sector_at(fh, name, physical, sector, n - 1).await?.is_some()
    {
        return Ok(Ok((n * s, format!("getstorageinfo ({n} sectors; its last sector reads)"))));
    }
    let found = probe_end(fh, name, physical, sector, BOOT_AREA_MAX / s).await?;
    let said = found.said();
    let Some(n) = found.sectors else {
        return Ok(Err(format!(
            "{said} found no refusal up to {} sectors: the programmer doesn't refuse reads past the end, so its size can't be found",
            BOOT_AREA_MAX / s
        )));
    };
    let beside = match (blocks, reported) {
        (None, _) => "getstorageinfo reports no size for it".to_string(),
        (Some(b), _) if Some(b) == users => format!("getstorageinfo answers with the user area's {b} sectors"),
        (Some(b), Some(_)) if b == n => format!("getstorageinfo reports the same {b} sectors"),
        (Some(b), _) => format!("getstorageinfo reports {b} sectors"),
    };
    Ok(Ok((n * s, format!("{said}; {beside}"))))
}

/// What a run collected, also when it failed part way.
#[derive(Default)]
struct Found {
    images: Vec<(DiskRecord, u64, String)>,
    incomplete: Vec<(String, String, String)>,
    skipped: Vec<Skipped>,
    sizes: Vec<AreaSize>,
}

async fn read_all<T: Transport, E: AsyncImageEdge>(
    fh: &mut Firehose<T>,
    scope: &Scope,
    opts: Options,
    edge: &mut E,
    found: &mut Found,
) -> Result<Read, EmergencyError> {
    let Found {
        images,
        incomplete,
        skipped,
        sizes,
    } = found;
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
    let (t, disk_size, source) = read_gpt(fh, sector, storage.as_ref(), edge).await?;
    edge.note(&format!(
        "GPT: {} partitions, disk {} bytes ({} sectors of {sector})",
        t.entries.len(),
        disk_size,
        disk_size / s
    ));
    edge.note(&size_line(USER_AREA_NAME, USER_AREA, disk_size, &source));
    sizes.push(AreaSize {
        name: USER_AREA_NAME.into(),
        physical: USER_AREA,
        bytes: disk_size,
        source,
    });
    edge.note(&render_table(&t, sector));

    let reserved = |n: &str| n == USER_AREA_NAME || BOOT_AREAS.iter().any(|(_, b)| *b == n);
    let wanted: Vec<Image> = match scope {
        Scope::GptOnly | Scope::BootAreas | Scope::BootAreaSizes => vec![],
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
                if !storable_name(n) || reserved(n) || v.iter().any(|i| &i.disk.name == n) {
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
        store_one(fh, img, opts, edge, images, incomplete).await?;
    }
    // The boot areas after the user area: a problem with them never costs it.
    if matches!(scope, Scope::WholeDisk | Scope::BootAreas | Scope::BootAreaSizes) {
        let search = *scope == Scope::BootAreaSizes;
        if search {
            edge.note("finding the boot areas' sizes by reading single sectors, whatever getstorageinfo reports; nothing is stored");
        }
        for (physical, name) in BOOT_AREAS {
            let size = match boot_area_size(fh, physical, name, sector, storage.as_ref(), search).await {
                Ok(z) => z,
                Err(e) => {
                    skipped.push(Skipped {
                        name: name.into(),
                        physical,
                        why: format!("finding its size failed: {e}"),
                    });
                    return Err(e);
                }
            };
            match size {
                Ok((size, source)) => {
                    edge.note(&size_line(name, physical, size, &source));
                    sizes.push(AreaSize {
                        name: name.into(),
                        physical,
                        bytes: size,
                        source,
                    });
                    if search {
                        continue;
                    }
                    let img = Image {
                        disk: Disk {
                            index: physical,
                            name: name.into(),
                            size,
                            sector,
                        },
                        start_sector: 0,
                    };
                    store_one(fh, &img, opts, edge, images, incomplete).await?;
                }
                Err(why) => {
                    edge.note(&format!("{name} (physical partition {physical}) isn't read: {why}"));
                    skipped.push(Skipped {
                        name: name.into(),
                        physical,
                        why,
                    });
                }
            }
        }
    }
    // Asked for the boot areas only, and the device has none that reads: the
    // run read nothing it was asked for.
    if *scope == Scope::BootAreas && images.is_empty() {
        let why: Vec<String> = skipped.iter().map(|k| format!("{}: {}", k.name, k.why)).collect();
        return Err(EmergencyError::Storage(format!("no eMMC boot area was read ({})", why.join("; "))));
    }
    if *scope == Scope::BootAreaSizes && sizes.iter().all(|z| z.physical == USER_AREA) {
        let why: Vec<String> = skipped.iter().map(|k| format!("{}: {}", k.name, k.why)).collect();
        return Err(EmergencyError::Storage(format!("no eMMC boot area's size was found ({})", why.join("; "))));
    }
    Ok((storage, sector, disk_size, t))
}

/// The line that says an area's size and where it came from.
fn size_line(name: &str, physical: u32, bytes: u64, source: &str) -> String {
    format!("{name} (physical partition {physical}): {bytes} bytes, size from {source}")
}
async fn store_one<T: Transport, E: AsyncImageEdge>(
    fh: &mut Firehose<T>,
    img: &Image,
    opts: Options,
    edge: &mut E,
    images: &mut Vec<(DiskRecord, u64, String)>,
    incomplete: &mut Vec<(String, String, String)>,
) -> Result<(), EmergencyError> {
    edge.note(&format!(
        "reading {} ({} bytes from sector {} of physical partition {}), every range twice",
        img.disk.name, img.disk.size, img.start_sector, img.disk.index
    ));
    let (rec, file) = store_image(fh, img, opts, edge, incomplete).await?;
    edge.note(&format!("{}: {} bytes, sha256 {}, checked", img.disk.name, img.disk.size, rec.sha256_hex()));
    images.push((rec, img.start_sector, file));
    Ok(())
}
