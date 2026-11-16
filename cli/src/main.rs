// SPDX-License-Identifier: GPL-2.0-or-later
//! glassine-install: the installer's command line, a thin client of the core.
//! All file reading happens here; the core only sees text and bytes.

use glassine_installer::facts::Snapshot;
use glassine_installer::format1::{self, Description};
use glassine_installer::inspect::{self, Host, Inspected};
use glassine_installer::release::Release;
use glassine_installer::validate::{self, Loaded};
use glassine_installer::{lock, plan, recognize, report};
use glassine_installer_pc as pc;
use glassine_installer_usb as usb;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod backup;
mod emergency;
mod emergency_read;
mod emergency_try;
mod host_setup;
mod install;
mod restore;
mod supplied_check;

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));

const OWN_REPOSITORY: &str = "GlassineOS/installer";

pub(crate) const USAGE: &str = "usage:
  glassine-install validate FILE... [--lock LOCKFILE [--require-pins]]
  glassine-install inspect [--json] [--out FACTS.json] [--reboot | --pc [--root DIR]] [--devices DIR]
  glassine-install check [--facts FACTS.json | --reboot | --pc [--root DIR]] [--release RELEASE.json] [--json] [--devices DIR]
  glassine-install plan --dry-run [--facts FACTS.json | --reboot | --pc [--root DIR]] [--choice K=V]...
                        [--hardware H=yes|no]... [--developer] [--free BYTES] [--job JOB] [--release RELEASE.json]
                        [--json] [--devices DIR]
  glassine-install backup --ram IMAGE --out DIR [--accept-slot-flags] [--devices DIR]
  glassine-install emergency-info [--json] [--serial]
  glassine-install emergency-read --programmer FILE [--accept-unknown] [--device ID] [--devices DIR]
                                 (--only-gpt | --find-boot-area-sizes | --out DIR [--only-boot-areas | --partition NAME...])
  glassine-install supplied-check [--device ID] [--file ID] [--reported KEY=VALUE]... [--json] [--devices DIR] FILE
  glassine-install host-setup [--print]
  glassine-install install --description FILE --release-dir DIR [--backup DIR] [--developer] [--record PATH]
                           [--facts FACTS.json | --pc [--root DIR]] [--choice K=V]... [--hardware H=yes|no]... [--job JOB]
  glassine-install resume --record PATH [--backup DIR]
  glassine-install restore --pc --backup DIR [--disk N] [--root DIR]
  glassine-install schema
  glassine-install lock-unpinned LOCKFILE

Descriptions come from the copies built into this program unless --devices names a directory.
inspect reads the one device connected in fastboot mode and writes nothing to it (getvar only;
--reboot restarts it at the end). With --pc it reads the PC it runs on instead (Linux only): the
firmware, its variables, every disk's partition table and file systems, all read-only, nothing
mounted; --root DIR reads a tree standing for one (sys/, dev/, proc/) instead of /. The PC is
never inspected without --pc: no device connected doesn't mean this computer. check and plan read a facts snapshot, or without --facts
inspect the connected device first. A boot loader that only answers in a fresh start of its
mode needs restarting before the next live command: use --reboot, or keep the facts with --out
and pass them with --facts. --release reads the installer release's manifest (the least sizes
its artifacts need); without it, those sizes are not known yet and never refuse a plan.

backup (Linux) reads the whole internal storage of the one device connected in fastboot mode
into DIR (new or empty): it boots the RAM environment IMAGE from the boot loader's memory, waits
for it on USB, reads every disk read-only into a gzip file, checks each against the device's
hashes and the stored copy, writes manifest.json and restarts the device. It sends download and
boot, which no other command does. On a boot loader with two slots it reads the active slot's
flags first: booting changes nothing when the slot is marked successful and not unbootable; in
any other state (or when the flags aren't published) it is refused unless --accept-slot-flags.

emergency-info reads what the boot ROM of the one device connected in its emergency download
mode (Qualcomm, USB 05c6:9008) reports, in Sahara's command mode: the Sahara version, the chip
(its hardware id and, when known, its name), the hash of the maker's key and the boot ROM's
version. It sends queries only, never a program, and leaves the device waiting in that mode:
hold its power key ~10 s to restart it. The serial number is unique to the unit and shown only
with --serial. Linux needs no driver, only access (host-setup below, or root); Windows needs the
WinUSB driver for the device, installed once with Zadig.

emergency-read reads the storage of that device through a programmer the person supplies:
--programmer names the file, and nothing else is ever looked for or sent. Before the device is
touched, the file is checked as supplied-check does: a known file is sent, one that fits but isn't
known only with --accept-unknown, any other never (exit status 1, with why). It reads the boot
ROM's facts and compares the chip and key hash it reports with the description (a chip this
installer doesn't know, or a device that isn't the described one, gets nothing: the run stops
and says what was reported and what was expected), sends the file, waits for the
programmer's first answer (none: the file isn't working on this device), then only lists, reads
and restarts: the partition table (GPT; when the primary at LBA 1 is damaged, the backup at the
disk's last sector, and only both bad stop the run), then with --out DIR (new or empty) the
partitions named by --partition, or without --partition the whole disk: the eMMC's user area and
its two boot areas when the device has them (emmc-boot1, emmc-boot2; RPMB never).
--only-boot-areas reads just the two boot areas, the same way (the partition table is still read
first); a device that has neither ends the run with why. The size of each area is printed with
where it came from: getstorageinfo, a GPT header, or the single-sector search (reads past the
end must be refused; each search read waits at most 5 s, and one not answered ends the run).
A device whose description says its programmer hangs on a read past the end
(sizes-reported-only) never gets the search: an area getstorageinfo and the GPT don't size isn't
read, and the manifest says why. --find-boot-area-sizes stores nothing: it reads the partition
table and finds the boot areas' sizes by that search whatever getstorageinfo reports, printing
both, then restarts the device; for such a device it is refused before anything is sent.
Each image is read twice range by range, compared, stored as NAME.img.gz.partial, read back and
checked, and only then renamed to NAME.img.gz; one that fails becomes NAME.img.gz.incomplete. manifest.json, written last, lists
them; a run that stopped before it leaves no NAME.img.gz that wasn't checked. At the end the
device is restarted. On a device not read this way before, take it in steps: --only-gpt first,
then one small partition (--partition devinfo --out DIR), then the boot areas (--only-boot-areas
--out DIR), then the whole disk. GLASSINE_USB_TRACE=1 logs every USB transfer.

supplied-check checks FILE as a file the device's description needs from the person (a programmer
for its emergency mode), as the installer does before sending one: its checksum against the files
known to work and known not to, then that it is made for the chip and key the description names;
--reported gives what the device reported (chip=HEX, pk_hash=HEX), which must match too. It reads
the file only and sends nothing anywhere. Exit status 0: a known file; 2: it fits but isn't known
(sent only after the person confirms); 1: refused (known not to work, with why; another chip or
key; or not a programmer).

install plans the job for the description's device (from --facts, or the connected device's
boot loader, restarted afterwards), shows the plan and the release's artifacts (release.json in
--release-dir) and runs it only after the person types \"install\". A plan with untried write
steps runs only with --developer. The record (default install-record.json; beside it
PATH.inputs.json) holds every step's intent before it starts and its outcome with evidence; it is
replaced whole on every change; PATH.kept/ holds what a step read before it first changed
it (a kept Android's boot image), so a resume changes the original again. Creating PATH.cancel stops the run after the next step that is safe
to stop after. resume --record PATH goes on from the record: it refuses another plan, description
or artifact, checks a cut step before running it again, and never rewrites a boot loader that
already answers as the new one. The installer restarts the device itself where it can (adb reboot
bootloader from stock Android when adb is on PATH) and otherwise says what to press; a running adb
server can hold the boot loader's interface (adb kill-server frees it). After the last restart the
installer can't see GlassineOS yet: it says what to check.

install --pc installs on the PC it runs on (Linux only), from a live read of it (--root DIR: a
tree standing for one): it writes only the plan's target disk, opened exclusively and refused
while a file system on it is mounted or swap is on it, and the firmware's boot entries. Its first
step backs up every disk of the plan into --backup DIR (each disk as disk<N>.img.gz, read back
and compared with the disk read again, then manifest.json), which with the record must be on
another disk. The image holds the partition table, the space outside the partitions, every
partition it can't read whole, and of ext4, btrfs, NTFS and FAT only the blocks in use (by each
file system's own metadata; one that fails a check, or wasn't shut down cleanly, is kept whole);
blocks of zeros are recorded as zeros. Past 2 GiB, an image goes on in disk<N>.img.gz.1, .2, ...
(FAT32 holds no file of 4 GiB). With
--choice backup=off it keeps only the firmware's boot entries beside the record. Then it writes
the partition table, the system partition, the boot partition and one boot entry, first in the
boot order, each read back past the cache. resume goes on from the record after a cut.

restore --pc puts such a backup back (Linux only): onto the disk it was made of, or --disk N of
the same size and sector size, refused while that disk is in use or holds the backup. After
\"restore\" is typed it checks the whole stored copy against the manifest, then writes each
range only where the disk differs (the first MiB, the primary partition table, last), reads it
back, and puts back the firmware's boot entries and boot order the backup holds (entries added
since stay, outside the boot order). The file systems come back as they were; their free space
is not kept and holds whatever the disk held. A cut restore is run again with the same command.

host-setup sets this computer up once to reach the devices' USB modes, from the descriptions'
modes. On Linux, as root (sudo glassine-install host-setup), it writes the udev rules file
/etc/udev/rules.d/70-glassine-installer.rules, reloads udev and re-triggers the USB devices
already connected: the person logged in at the computer may open each mode's USB device without
root (TAG uaccess), ModemManager leaves them alone, and the kernel's qcserial driver lets go of the
emergency download mode (05c6:9008) when it binds, for that id only, since a browser can't take it
from a kernel driver. With --print it writes the file to stdout and what to run to stderr, and
changes nothing. On Windows it lists the USB ids that need the WinUSB driver and how to install
it with Zadig; nothing else needs setting up.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("validate") => validate_cmd(&args[1..]),
        Some("inspect") => inspect_cmd(&args[1..]),
        Some("check") => check_cmd(&args[1..]),
        Some("plan") => plan_cmd(&args[1..]),
        Some("backup") => backup_cmd(&args[1..]),
        Some("emergency-info") => emergency_cmd(&args[1..]),
        Some("emergency-read") => emergency_read_cmd(&args[1..]),
        Some("emergency-try-program") => emergency_try_cmd(&args[1..]),
        Some("supplied-check") => supplied_check::cmd(&args[1..]),
        Some("host-setup") => host_setup_cmd(&args[1..]),
        Some("install") => install::install_cmd(&args[1..]),
        Some("resume") => install::resume_cmd(&args[1..]),
        Some("restore") => restore::restore_cmd(&args[1..]),
        Some("schema") => {
            println!("{}", serde_json::to_string_pretty(&format1::json_schema()).unwrap());
            Ok(ExitCode::SUCCESS)
        }
        Some("lock-unpinned") => unpinned_cmd(&args[1..]),
        Some("-h" | "--help" | "help") => {
            println!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        _ => Err(USAGE.to_string()),
    };
    match r {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::from(64)
        }
    }
}

fn read(p: &Path) -> Result<String, String> {
    std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))
}

struct Opts {
    flags: Vec<String>,
    values: BTreeMap<String, Vec<String>>,
    rest: Vec<String>,
}

fn opts(args: &[String], flags: &[&str], valued: &[&str]) -> Result<Opts, String> {
    let mut o = Opts {
        flags: vec![],
        values: BTreeMap::new(),
        rest: vec![],
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if flags.contains(&a.as_str()) {
            o.flags.push(a.clone());
        } else if valued.contains(&a.as_str()) {
            let v = it.next().ok_or(format!("{a} needs a value\n\n{USAGE}"))?;
            o.values.entry(a.clone()).or_default().push(v.clone());
        } else if a.starts_with("--") {
            return Err(format!("unknown option {a}\n\n{USAGE}"));
        } else {
            o.rest.push(a.clone());
        }
    }
    Ok(o)
}

impl Opts {
    fn has(&self, f: &str) -> bool {
        self.flags.iter().any(|x| x == f)
    }
    fn one(&self, k: &str) -> Option<&String> {
        self.values.get(k).and_then(|v| v.last())
    }
    fn all(&self, k: &str) -> &[String] {
        self.values.get(k).map(Vec::as_slice).unwrap_or(&[])
    }
}

fn copies_next_to(lock_path: &Path) -> Result<Vec<(PathBuf, Vec<u8>)>, String> {
    let dir = lock_path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    toml_files(dir)?
        .into_iter()
        .map(|p| std::fs::read(&p).map(|b| (p.clone(), b)).map_err(|e| format!("{}: {e}", p.display())))
        .collect()
}

fn named(copies: &[(PathBuf, Vec<u8>)]) -> Vec<(String, Vec<u8>)> {
    copies
        .iter()
        .map(|(p, b)| (p.file_name().unwrap().to_string_lossy().into_owned(), b.clone()))
        .collect()
}

fn validate_cmd(args: &[String]) -> Result<ExitCode, String> {
    let o = opts(args, &["--require-pins"], &["--lock"])?;
    let mut failed = false;
    let mut too_new = false;
    let mut valid: Vec<(String, Description)> = Vec::new();
    let mut files: Vec<PathBuf> = o.rest.iter().map(PathBuf::from).collect();
    if let Some(l) = o.one("--lock") {
        let lock_path = Path::new(l);
        let copies = copies_next_to(lock_path)?;
        let checked = lock::check(&read(lock_path)?, OWN_REPOSITORY, &named(&copies), o.has("--require-pins"));
        for p in &checked.problems {
            println!("{}: {}: {}", lock_path.display(), p.path, p.message);
        }
        for (repo, path) in &checked.unpinned {
            println!("{}: unpinned: {repo} {path} (to be pinned once that repository has it)", lock_path.display());
        }
        failed |= !checked.problems.is_empty();
        for (p, _) in copies {
            if !files.contains(&p) {
                files.push(p);
            }
        }
    }
    if files.is_empty() {
        return Err(USAGE.into());
    }
    for f in &files {
        match validate::load(&read(f)?) {
            Loaded::Valid(d) => valid.push((f.display().to_string(), *d)),
            Loaded::TooNew { format } => {
                println!(
                    "{}: format {format} is newer than this installer reads (up to {}); update the installer",
                    f.display(),
                    validate::READS_UP_TO
                );
                too_new = true;
            }
            Loaded::Invalid(ps) => {
                failed = true;
                for p in ps {
                    println!("{}: {}: {}", f.display(), p.path, p.message);
                }
            }
        }
    }
    let refs: Vec<&Description> = valid.iter().map(|(_, d)| d).collect();
    for (id, p) in validate::check_set(&refs) {
        failed = true;
        let file = valid.iter().find(|(_, d)| d.id == id).map(|(f, _)| f.clone()).unwrap_or(id);
        println!("{file}: {}: {}", p.path, p.message);
    }
    if !failed && !too_new {
        println!("{} description(s) valid", valid.len());
    }
    Ok(if failed {
        ExitCode::from(1)
    } else if too_new {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    })
}

fn toml_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml") && p.file_name().is_some_and(|n| n != "lock.toml"))
        .collect();
    v.sort();
    Ok(v)
}

fn host_setup_cmd(args: &[String]) -> Result<ExitCode, String> {
    let ds = descriptions(&opts(&[], &[], &[])?)?;
    let refs: Vec<&Description> = ds.iter().collect();
    host_setup::cmd(args, &refs)
}

/// The descriptions to recognise against: valid ones only; a newer format is
/// refused by name and the rest go on (Scenario 2).
fn descriptions(o: &Opts) -> Result<Vec<Description>, String> {
    let sources: Vec<(String, String)> = match o.one("--devices") {
        Some(dir) => toml_files(Path::new(dir))?
            .into_iter()
            .map(|p| Ok((p.display().to_string(), read(&p)?)))
            .collect::<Result<_, String>>()?,
        None => EMBEDDED.iter().map(|(n, t)| (n.to_string(), t.to_string())).collect(),
    };
    let mut out = Vec::new();
    for (name, text) in sources {
        match validate::load(&text) {
            Loaded::Valid(d) => out.push(*d),
            Loaded::TooNew { format } => eprintln!("{name}: format {format} is newer than this installer reads; skipped (update the installer)"),
            Loaded::Invalid(ps) => eprintln!("{name}: invalid ({} problems); skipped, run validate", ps.len()),
        }
    }
    Ok(out)
}

/// Exit code for a device that couldn't be inspected (none, busy, not fresh,
/// a timeout); the message says which.
const DEVICE_FAILED: u8 = 5;

/// Inspects the one connected fastboot device.
fn live(ds: &[&Description], reboot: bool) -> Result<Inspected, String> {
    let info = usb::find().map_err(|e| e.to_string())?;
    let (t, ids) = usb::open(&info).map_err(|e| match e {
        usb::OpenError::Permission(_) => format!(
            "{e}: on Linux set this computer up once with sudo glassine-install host-setup (or run this as root); \
             on Windows the device needs the WinUSB driver (glassine-install host-setup says how)"
        ),
        e => e.to_string(),
    })?;
    let host = Host {
        platform: std::env::consts::OS.to_string(),
        connections: vec!["usb-native".into()],
    };
    let got = pollster::block_on(inspect::inspect(ds, t, &ids, &host, reboot)).map_err(|e| e.to_string())?;
    if let Some(e) = &got.reboot_error {
        eprintln!("the facts were read, but the reboot at the end failed: {e}");
    }
    Ok(got)
}

/// Whether the PC this runs on is to be inspected, and below which root.
fn pc_root(o: &Opts) -> Option<PathBuf> {
    match (o.one("--root"), o.has("--pc")) {
        (Some(r), _) => Some(PathBuf::from(r)),
        (None, true) => Some(PathBuf::from("/")),
        _ => None,
    }
}

/// Inspects the PC (or the tree standing for it); its notes go to stderr.
fn live_pc(root: &Path) -> Result<Snapshot, String> {
    let got = pc::inspect(root)?;
    for n in &got.notes {
        eprintln!("note: {n}");
    }
    Ok(got.snapshot)
}

/// The snapshot from --facts, or a live inspect without it. `Err(Ok(code))`
/// is a device that couldn't be inspected (already reported).
fn facts(o: &Opts, ds: &[&Description]) -> Result<Result<Snapshot, ExitCode>, String> {
    if o.one("--facts").is_some() && (pc_root(o).is_some() || o.has("--reboot")) {
        return Err("--facts reads a snapshot: it doesn't go with --pc, --root or --reboot".into());
    }
    if pc_root(o).is_some() && o.has("--reboot") {
        return Err("--reboot restarts a device; it doesn't go with --pc".into());
    }
    if let Some(root) = pc_root(o) {
        return match live_pc(&root) {
            Ok(s) => Ok(Ok(s)),
            Err(e) => {
                eprintln!("{e}");
                Ok(Err(ExitCode::from(DEVICE_FAILED)))
            }
        };
    }
    match o.one("--facts") {
        Some(f) => Snapshot::parse(&read(Path::new(f))?).map(Ok).map_err(|e| format!("{f}: {e}")),
        None => match live(ds, o.has("--reboot")) {
            Ok(i) => Ok(Ok(i.snapshot)),
            Err(e) => {
                eprintln!("{e}");
                Ok(Err(ExitCode::from(DEVICE_FAILED)))
            }
        },
    }
}

fn inspect_cmd(args: &[String]) -> Result<ExitCode, String> {
    let o = opts(args, &["--json", "--reboot", "--pc"], &["--out", "--devices", "--root"])?;
    if let Some(root) = pc_root(&o) {
        if o.has("--reboot") {
            return Err("--reboot restarts a device; it doesn't go with --pc".into());
        }
        let s = match live_pc(&root) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{e}");
                return Ok(ExitCode::from(DEVICE_FAILED));
            }
        };
        let json = serde_json::to_string_pretty(&s).unwrap() + "\n";
        if let Some(out) = o.one("--out") {
            std::fs::write(out, &json).map_err(|e| format!("{out}: {e}"))?;
        }
        if o.has("--json") {
            print!("{json}");
        } else {
            print!("{}", render_pc(&s));
            if let Some(out) = o.one("--out") {
                println!("facts written to {out}");
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    let ds = descriptions(&o)?;
    let refs: Vec<&Description> = ds.iter().collect();
    let got = match live(&refs, o.has("--reboot")) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            return Ok(ExitCode::from(DEVICE_FAILED));
        }
    };
    let json = serde_json::to_string_pretty(&got.snapshot).unwrap() + "\n";
    if let Some(out) = o.one("--out") {
        std::fs::write(out, &json).map_err(|e| format!("{out}: {e}"))?;
    }
    if o.has("--json") {
        print!("{json}");
    } else {
        let usb = got.snapshot.usb.as_ref().unwrap();
        println!("fastboot device {}:{}", usb.vendor_id, usb.product_id);
        match got.told_by.as_slice() {
            [] => println!("framing: {}", got.framing.name()),
            t => println!("framing: {} (told by {})", got.framing.name(), t.join(", ")),
        }
        for (k, v) in &got.snapshot.fastboot.as_ref().unwrap().variables {
            println!("  {k}: {}", v.as_deref().unwrap_or("(FAIL)"));
        }
        if let Some(out) = o.one("--out") {
            println!("facts written to {out}");
        }
        if got.rebooted {
            println!("the device is restarting");
        }
    }
    Ok(if got.reboot_error.is_some() {
        ExitCode::from(DEVICE_FAILED)
    } else {
        ExitCode::SUCCESS
    })
}

fn backup_cmd(args: &[String]) -> Result<ExitCode, String> {
    let o = opts(args, &["--accept-slot-flags"], &["--ram", "--out", "--devices"])?;
    let (Some(ram), Some(out)) = (o.one("--ram"), o.one("--out")) else {
        return Err(format!("backup needs --ram IMAGE and --out DIR\n\n{USAGE}"));
    };
    if !o.rest.is_empty() {
        return Err(USAGE.into());
    }
    let image = std::fs::read(ram).map_err(|e| format!("{ram}: {e}"))?;
    let ds = descriptions(&o)?;
    let refs: Vec<&Description> = ds.iter().collect();
    match backup::run(&refs, &image, Path::new(out), o.has("--accept-slot-flags")) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(e) => {
            eprintln!("{e}");
            Ok(ExitCode::from(DEVICE_FAILED))
        }
    }
}

/// The PC's facts in short, for a person.
fn render_pc(s: &Snapshot) -> String {
    let mut o = String::new();
    if let Some(f) = &s.firmware {
        o += &format!(
            "firmware: {}{}{}\n",
            f.kind,
            f.secure_boot.as_deref().map(|x| format!(", secure boot {x}")).unwrap_or_default(),
            f.variables.as_deref().map(|x| format!(", variables {x}")).unwrap_or_default()
        );
        for e in &f.boot_entries {
            o += &format!("  boot entry {}: {}\n", e.number, e.label.as_deref().unwrap_or(""));
        }
    }
    if let Some(c) = &s.cpu {
        o += &format!("cpu: {}\n", c.arch);
    }
    let st = s.storage.as_ref();
    let installer = st.and_then(|x| x.installer_disk.as_ref());
    for d in st.map(|x| x.disks.as_slice()).unwrap_or_default() {
        let own = match installer {
            Some(i) if i.index == d.index => format!(" (the installer runs from it, told by its {})", i.by),
            _ => String::new(),
        };
        o += &format!(
            "disk {}: {}{}, {} bytes, table {}{own}\n",
            d.index,
            d.kind,
            if d.removable { ", removable" } else { "" },
            d.size,
            d.table.as_deref().unwrap_or("none")
        );
        for p in &d.partitions {
            let fs = p.fs.as_ref().map(|f| match f.used {
                Some(u) => format!(", {} with {u} bytes used", f.kind),
                None => format!(", {}", f.kind),
            });
            o += &format!(
                "  partition {}: {} bytes{}{}\n",
                p.index,
                p.size,
                p.type_guid.as_deref().map(|t| format!(", type {t}")).unwrap_or_default(),
                fs.unwrap_or_default()
            );
        }
        if !d.free.is_empty() {
            o += &format!("  free: {}\n", d.free.iter().map(|n| format!("{n} bytes")).collect::<Vec<_>>().join(", "));
        }
    }
    for (i, sys) in s.systems.iter().enumerate() {
        o += &format!("system {}: {} on disk {}\n", i + 1, sys.kind, sys.disk);
    }
    o
}

fn plan_input(o: &Opts) -> Result<plan::Input, String> {
    let mut input = plan::Input {
        developer: o.has("--developer"),
        job: o.one("--job").cloned(),
        ..Default::default()
    };
    for c in o.all("--choice") {
        let (k, v) = c.split_once('=').ok_or(format!("--choice takes KEY=VALUE, not {c:?}"))?;
        input.choices.insert(k.into(), v.into());
    }
    for h in o.all("--hardware") {
        let (k, v) = h.split_once('=').ok_or(format!("--hardware takes NAME=yes|no, not {h:?}"))?;
        let b = match v {
            "yes" => true,
            "no" => false,
            _ => return Err(format!("--hardware {k}= takes yes or no")),
        };
        input.hardware.insert(k.into(), b);
    }
    if let Some(n) = o.one("--free") {
        input.free_bytes = Some(n.parse().map_err(|_| format!("--free takes a number of bytes, not {n:?}"))?);
    }
    input.release = release(o)?;
    Ok(input)
}

/// The release manifest of --release, if given.
fn release(o: &Opts) -> Result<Option<Release>, String> {
    o.one("--release")
        .map(|f| Release::parse(&read(Path::new(f))?).map_err(|e| format!("{f}: {e}")))
        .transpose()
}

fn check_cmd(args: &[String]) -> Result<ExitCode, String> {
    let o = opts(args, &["--json", "--reboot", "--pc"], &["--facts", "--devices", "--root", "--release"])?;
    let input = plan::Input {
        release: release(&o)?,
        ..Default::default()
    };
    let ds = descriptions(&o)?;
    let refs: Vec<&Description> = ds.iter().collect();
    let f = match facts(&o, &refs)? {
        Ok(f) => f,
        Err(code) => return Ok(code),
    };
    let rec = recognize::recognize(&refs, &f);
    let p = plan::plan(&refs, &f, &rec, &input);
    let r = report::report(&refs, &f, &rec, Some(&p));
    if o.has("--json") {
        println!("{}", serde_json::to_string_pretty(&r).unwrap());
    } else {
        print!("{}", report::render(&r));
    }
    Ok(if r.supported { ExitCode::SUCCESS } else { ExitCode::from(3) })
}

fn plan_cmd(args: &[String]) -> Result<ExitCode, String> {
    let o = opts(
        args,
        &["--json", "--dry-run", "--developer", "--reboot", "--pc"],
        &["--facts", "--devices", "--choice", "--hardware", "--free", "--job", "--root", "--release"],
    )?;
    if !o.has("--dry-run") {
        return Err("only the dry run exists: plan --dry-run".into());
    }
    let input = plan_input(&o)?;
    let ds = descriptions(&o)?;
    let refs: Vec<&Description> = ds.iter().collect();
    let f = match facts(&o, &refs)? {
        Ok(f) => f,
        Err(code) => return Ok(code),
    };
    let rec = recognize::recognize(&refs, &f);
    let p = plan::plan(&refs, &f, &rec, &input);
    if o.has("--json") {
        println!("{}", serde_json::to_string_pretty(&p).unwrap());
    } else {
        print!("{}", plan::render(&p, rec.matched(&refs)));
    }
    Ok(match p.status {
        plan::Status::Ready => ExitCode::SUCCESS,
        plan::Status::Refused => ExitCode::from(3),
        _ => ExitCode::from(4),
    })
}

fn emergency_cmd(args: &[String]) -> Result<ExitCode, String> {
    let o = opts(args, &["--json", "--serial"], &[])?;
    if !o.rest.is_empty() {
        return Err(USAGE.into());
    }
    let r = emergency::open().and_then(|(t, ids)| pollster::block_on(emergency::run(t, &ids, o.has("--json"), o.has("--serial"))));
    match r {
        Ok(out) => {
            print!("{out}");
            eprintln!("{}", emergency::LEAVE);
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("{e}");
            Ok(ExitCode::from(DEVICE_FAILED))
        }
    }
}

fn emergency_read_cmd(args: &[String]) -> Result<ExitCode, String> {
    let o = opts(
        args,
        &["--only-gpt", "--find-boot-area-sizes", "--only-boot-areas", "--accept-unknown"],
        &["--programmer", "--out", "--partition", "--device", "--devices"],
    )?;
    if !o.rest.is_empty() {
        return Err(USAGE.into());
    }
    let Some(programmer) = o.one("--programmer") else {
        return Err(format!(
            "emergency-read needs --programmer FILE: the person's own programmer for the device\n\n{USAGE}"
        ));
    };
    let scope = emergency_read::scope(
        o.has("--only-gpt"),
        o.has("--find-boot-area-sizes"),
        o.has("--only-boot-areas"),
        o.all("--partition"),
        o.one("--out").map(String::as_str),
    )?;
    let ds = descriptions(&o)?;
    let d = supplied_check::pick(&ds, o.one("--device").map(String::as_str))?;
    let file = supplied_check::file_of(d, None)?;
    // The file is checked before anything opens the device (004 FR-004).
    let bytes = match emergency_read::load(Path::new(programmer), d, &file, o.has("--accept-unknown")) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("{e}");
            return Ok(ExitCode::from(supplied_check::REFUSED));
        }
    };
    match emergency_read::run(&bytes, d, &file, o.one("--out").map(Path::new), &scope) {
        Ok(out) => {
            print!("{out}");
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("{e}");
            Ok(ExitCode::from(DEVICE_FAILED))
        }
    }
}

fn emergency_try_cmd(args: &[String]) -> Result<ExitCode, String> {
    let o = opts(args, &[], &[])?;
    let [image] = o.rest.as_slice() else {
        return Err("usage: glassine-install emergency-try-program IMAGE".into());
    };
    let img = std::fs::read(image).map_err(|e| format!("{image}: {e}"))?;
    let (mut t, _ids) = emergency::open()?;
    // One timeout as long as the whole listening window: the transport
    // closes after its first timeout.
    t.set_timeout(emergency_try::LISTEN);
    let r = pollster::block_on(emergency_try::run(t, &img, emergency_try::LISTEN));
    match r {
        Ok(out) => {
            print!("{out}");
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("{e}");
            Ok(ExitCode::from(DEVICE_FAILED))
        }
    }
}

fn unpinned_cmd(args: &[String]) -> Result<ExitCode, String> {
    let l = args.first().ok_or(USAGE)?;
    let lock_path = Path::new(l);
    let copies = copies_next_to(lock_path)?;
    for (repo, path) in lock::check(&read(lock_path)?, OWN_REPOSITORY, &named(&copies), false).unpinned {
        println!("{repo} {path}");
    }
    Ok(ExitCode::SUCCESS)
}
