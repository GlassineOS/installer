// SPDX-License-Identifier: GPL-2.0-or-later
//! `glassine-install install` and `glassine-install resume`: the executor
//! (core `execute`) with this computer as its edge: the release from a
//! folder, the record in a file (written whole under a temporary name, then
//! renamed), the device over USB, `adb` for the one restart the core can't
//! send (stock Android into its boot loader), and the person's hands for
//! the rest.
//!
//! Beside the record, `RECORD.inputs.json` keeps what a resume needs that
//! the record only holds as hashes: the description file, the release
//! folder, the backup folder and whether it is a developer's run. A resume
//! checks them against the record's hashes (the executor refuses another
//! plan, description, artifact or backup: `--backup` on a resume must name
//! the backup the record holds).

use super::{Opts, USAGE, opts, pc, plan_input, read};
use flate2::read::GzDecoder;
use glassine_installer::artifacts::{ArtifactSource, Artifacts};
use glassine_installer::emergency_read::USER_AREA_NAME;
use glassine_installer::execute::{self, InstallEdge, Job, Link, Outcome, Reach};
use glassine_installer::facts::Snapshot;
use glassine_installer::format1::{Description, Mode, Step};
use glassine_installer::lock::sha256_hex;
use glassine_installer::plan::{self, Status};
use glassine_installer::record::{self, Backup, Confirmation, Record, RecordStore, StepState};
use glassine_installer::validate::{self, Loaded};
use glassine_installer::{recognize, release::Release};
use glassine_installer_usb as usb;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

/// How long the device may take to show up in a mode.
const REACH_WAIT: Duration = Duration::from_secs(300);
/// The RAM environment bounds one operation at 120 s.
const RAM_TRANSFER_TIMEOUT: Duration = Duration::from_secs(130);
/// What the person types to confirm.
const CONFIRM: &str = "install";

/// A release in a folder: `release.json` and the files it names.
struct DirSource(PathBuf);

impl DirSource {
    fn path(&self, file: &str) -> Result<PathBuf, String> {
        if file != glassine_installer::artifacts::MANIFEST && !glassine_installer::artifacts::plain_path(file) {
            return Err(format!("{file:?} is not a plain relative path"));
        }
        Ok(self.0.join(file))
    }
}

impl ArtifactSource for DirSource {
    fn origin(&self) -> String {
        self.0.display().to_string()
    }
    async fn size(&mut self, file: &str) -> Result<u64, String> {
        let p = self.path(file)?;
        std::fs::metadata(&p).map(|m| m.len()).map_err(|e| format!("{}: {e}", p.display()))
    }
    async fn read_at(&mut self, file: &str, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        use std::io::{Seek, SeekFrom};
        let p = self.path(file)?;
        let mut f = File::open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
        f.read_exact(buf).map_err(|e| format!("{}: {e}", p.display()))
    }
}

/// The record's file: a whole copy under `NAME.partial`, synced, then renamed.
struct FileStore(PathBuf);

impl FileStore {
    fn temp(&self) -> PathBuf {
        let mut s = self.0.clone().into_os_string();
        s.push(".partial");
        PathBuf::from(s)
    }
}

impl RecordStore for FileStore {
    fn write_temp(&mut self, bytes: &[u8]) -> Result<(), String> {
        let t = self.temp();
        let mut f = File::create(&t).map_err(|e| format!("{}: {e}", t.display()))?;
        f.write_all(bytes).and_then(|_| f.sync_all()).map_err(|e| format!("{}: {e}", t.display()))
    }
    fn commit(&mut self) -> Result<(), String> {
        std::fs::rename(self.temp(), &self.0).map_err(|e| format!("{}: {e}", self.0.display()))?;
        // Synced again by its new name: on FAT (a stick) the rename leaves the
        // directory entry to the file's next write-back.
        File::open(&self.0)
            .and_then(|f| f.sync_all())
            .map_err(|e| format!("{}: {e}", self.0.display()))?;
        if let Some(dir) = self.0.parent().filter(|d| !d.as_os_str().is_empty())
            && let Ok(d) = File::open(dir)
        {
            let _ = d.sync_all();
        }
        Ok(())
    }
    fn read(&mut self) -> Result<Option<Vec<u8>>, String> {
        match std::fs::read(&self.0) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", self.0.display())),
        }
    }
}

/// Where a step's originals are kept: `RECORD.kept/`.
fn kept_dir(record: &Path) -> PathBuf {
    let mut s = record.as_os_str().to_owned();
    s.push(".kept");
    PathBuf::from(s)
}

fn inputs_path(record: &Path) -> PathBuf {
    let mut s = record.as_os_str().to_owned();
    s.push(".inputs.json");
    PathBuf::from(s)
}

/// The backup the person made with `emergency-read --out DIR`: its manifest
/// (written last, only after every image was checked) and the user area.
fn verified_backup(dir: &Path) -> Result<Backup, String> {
    let m = dir.join("manifest.json");
    let bytes = std::fs::read(&m).map_err(|e| format!("{}: {e} (a finished backup has its manifest)", m.display()))?;
    let user = dir.join(format!("{USER_AREA_NAME}.img.gz"));
    if !user.exists() {
        return Err(format!("{} isn't there: the backup doesn't hold the whole user area", user.display()));
    }
    Ok(Backup {
        location: dir.display().to_string(),
        manifest_sha256: Some(sha256_hex(&bytes)),
    })
}

struct CliEdge<'a> {
    d: &'a Description,
    store: FileStore,
    kept: PathBuf,
    backup: Option<PathBuf>,
    cancel_file: PathBuf,
    shown: BTreeMap<String, u64>,
}

fn adb() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(if cfg!(windows) { "adb.exe" } else { "adb" }))
        .find(|p| p.is_file())
}

/// The description's mode the installer can't open itself (stock Android,
/// the emergency mode) whose USB ids are on the bus, if one is: read from
/// the bus alone, so no adb server is started to find out.
fn other_mode_on_bus(d: &Description) -> Option<String> {
    d.modes
        .iter()
        .filter(|m| m.kind != "fastboot" && m.kind != glassine_installer::ram_backup::RAM_MODE_KIND)
        .find(|m| {
            m.usb
                .as_ref()
                .is_some_and(|u| match (u16::from_str_radix(&u.vendor_id, 16), u16::from_str_radix(&u.product_id, 16)) {
                    (Ok(v), Ok(p)) => usb::count_ids(v, p).is_ok_and(|n| n > 0),
                    _ => false,
                })
        })
        .map(|m| m.id.clone())
}

fn open_error(e: usb::OpenError) -> String {
    match e {
        usb::OpenError::Busy(_) => {
            format!("{e}. A running adb server holds the interface of a device it knows: stop it with `adb kill-server` and the installer goes on")
        }
        usb::OpenError::Permission(_) => format!("{e}: set this computer up once with sudo glassine-install host-setup"),
        e => e.to_string(),
    }
}

impl CliEdge<'_> {
    /// The device's interface of `kind`, if one is there. An open that fails
    /// (busy, no access yet: udev applies it a moment after the device shows
    /// up) is kept as `last` and tried again until the deadline.
    fn try_kind(&self, kind: usb::Kind, last: &mut String, failed: &mut bool) -> Option<usb::UsbTransport> {
        match usb::find_kind(kind) {
            Ok(info) => match usb::open_kind(&info, kind) {
                Ok((mut t, _)) => {
                    if kind == usb::Kind::RamRead {
                        t.set_timeout(RAM_TRANSFER_TIMEOUT);
                    }
                    Some(t)
                }
                Err(e) => {
                    *last = open_error(e);
                    *failed = true;
                    None
                }
            },
            Err(usb::OpenError::NoDevice) => None,
            Err(e) => {
                *last = open_error(e);
                *failed = true;
                None
            }
        }
    }
}

impl InstallEdge for CliEdge<'_> {
    type Fb = usb::UsbTransport;
    type Ram = usb::UsbTransport;

    async fn reach(&mut self, mode: &Mode, how: Reach) -> Result<Link<Self::Fb, Self::Ram>, String> {
        let any = matches!(how, Reach::Already { .. });
        match &how {
            Reach::Already { text } => {
                if let Some(t) = text {
                    eprintln!("looking for the device ({}). {t}", mode.id);
                }
            }
            Reach::Sent { .. } => eprintln!("waiting for the device in {}", mode.id),
            Reach::Perform { transition, text } => {
                let t = self.d.transitions.iter().find(|t| t.id == *transition);
                let to_fastboot = t.is_some_and(|t| self.d.modes.iter().any(|m| m.id == t.to && m.kind == "fastboot"));
                let from_adb = t.is_some_and(|t| self.d.modes.iter().any(|m| m.id == t.from && m.kind == "android-adb"));
                match adb() {
                    Some(a) if to_fastboot && from_adb => {
                        eprintln!("{transition}: adb reboot bootloader");
                        let ok = Command::new(&a).args(["reboot", "bootloader"]).status().is_ok_and(|s| s.success());
                        if !ok {
                            eprintln!("adb couldn't restart the device. {}", text.clone().unwrap_or_default());
                        }
                        // The adb server stays running; it may hold the boot
                        // loader's interface (opening says so, with what to do).
                    }
                    _ => eprintln!("{transition}: {}", text.clone().unwrap_or_else(|| format!("bring the device into {}", mode.id))),
                }
            }
            Reach::AskPerson { text, .. } => eprintln!("{text}"),
        }
        let deadline = Instant::now() + REACH_WAIT;
        let mut last = String::from("nothing came up");
        loop {
            let mut failed = false;
            if (any || mode.kind == "fastboot")
                && let Some(t) = self.try_kind(usb::Kind::Fastboot, &mut last, &mut failed)
            {
                return Ok(Link::Fastboot(t));
            }
            if (any || mode.kind == glassine_installer::ram_backup::RAM_MODE_KIND)
                && let Some(t) = self.try_kind(usb::Kind::RamRead, &mut last, &mut failed)
            {
                return Ok(Link::Ram(t));
            }
            // A fastboot or RAM interface that is there but didn't open yet
            // isn't taken for another mode with the same USB ids.
            if any
                && !failed
                && let Some(m) = other_mode_on_bus(self.d)
            {
                return Ok(Link::Elsewhere(m));
            }
            if Instant::now() >= deadline {
                return Err(format!("the device didn't show up in {} within {} s ({last})", mode.id, REACH_WAIT.as_secs()));
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    async fn save(&mut self, r: &Record) -> Result<(), String> {
        record::save(&mut self.store, r)
    }

    async fn back_up(&mut self, _step: &Step) -> Result<Backup, String> {
        match &self.backup {
            Some(dir) => verified_backup(dir),
            None => Err(
                "the backup isn't made by install yet: make it with glassine-install emergency-read --programmer FILE --out DIR, then resume with --backup DIR"
                    .into(),
            ),
        }
    }

    async fn backup_bytes(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, String> {
        let dir = self.backup.as_ref().ok_or("no backup folder")?;
        let p = dir.join(format!("{USER_AREA_NAME}.img.gz"));
        let f = File::open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let mut z = GzDecoder::new(std::io::BufReader::new(f));
        std::io::copy(&mut (&mut z).take(offset), &mut std::io::sink()).map_err(|e| format!("{}: {e}", p.display()))?;
        let mut b = vec![0u8; len];
        z.read_exact(&mut b).map_err(|e| format!("{}: {e}", p.display()))?;
        Ok(b)
    }

    async fn backup_manifest(&mut self) -> Result<Option<String>, String> {
        let Some(dir) = &self.backup else { return Ok(None) };
        let m = dir.join("manifest.json");
        let bytes = std::fs::read(&m).map_err(|e| format!("{}: {e} (a finished backup has its manifest)", m.display()))?;
        Ok(Some(sha256_hex(&bytes)))
    }

    async fn keep(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
        if !glassine_installer::artifacts::plain_path(name) || name.contains('/') {
            return Err(format!("{name:?} isn't a plain file name"));
        }
        std::fs::create_dir_all(&self.kept).map_err(|e| format!("{}: {e}", self.kept.display()))?;
        let (to, temp) = (self.kept.join(name), self.kept.join(format!("{name}.partial")));
        let mut f = File::create(&temp).map_err(|e| format!("{}: {e}", temp.display()))?;
        f.write_all(bytes).and_then(|_| f.sync_all()).map_err(|e| format!("{}: {e}", temp.display()))?;
        std::fs::rename(&temp, &to).map_err(|e| format!("{}: {e}", to.display()))?;
        if let Ok(d) = File::open(&self.kept) {
            let _ = d.sync_all();
        }
        Ok(())
    }

    async fn kept(&mut self, name: &str) -> Result<Option<Vec<u8>>, String> {
        let p = self.kept.join(name);
        match std::fs::read(&p) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", p.display())),
        }
    }

    fn progress(&mut self, step: &str, done: u64, total: u64) {
        let last = self.shown.entry(step.to_string()).or_insert(0);
        if done == total || done >= *last + (64 << 20) || done < *last {
            *last = done;
            eprintln!("{step}: {done} of {total} bytes");
        }
    }

    fn note(&mut self, line: &str) {
        eprintln!("{line}");
    }

    fn cancel_requested(&mut self) -> bool {
        self.cancel_file.exists()
    }
}

fn load_description(path: &Path) -> Result<(Description, String), String> {
    let text = read(path)?;
    match validate::load(&text) {
        Loaded::Valid(d) => Ok((*d, sha256_hex(text.as_bytes()))),
        Loaded::TooNew { format } => Err(format!("{}: format {format} is newer than this installer reads", path.display())),
        Loaded::Invalid(ps) => Err(format!("{}: invalid ({} problems): run validate", path.display(), ps.len())),
    }
}

fn execute(
    d: &Description,
    sha: String,
    developer: bool,
    mut rec: Record,
    release: &Path,
    backup: Option<PathBuf>,
    record_path: &Path,
) -> Result<ExitCode, String> {
    let mut artifacts = pollster::block_on(Artifacts::open(DirSource(release.to_path_buf()))).map_err(|e| e.to_string())?;
    let mut cancel = record_path.as_os_str().to_owned();
    cancel.push(".cancel");
    let mut edge = CliEdge {
        d,
        store: FileStore(record_path.to_path_buf()),
        kept: kept_dir(record_path),
        backup,
        cancel_file: PathBuf::from(cancel),
        shown: BTreeMap::new(),
    };
    eprintln!("to stop after the next step that is safe to stop after, create {}", edge.cancel_file.display());
    let plan = rec.plan.clone();
    let job_id = serde_json::from_str::<serde_json::Value>(&plan)
        .ok()
        .and_then(|v| v.get("job").and_then(|j| j.as_str()).map(String::from))
        .ok_or("the record's plan names no job")?;
    let j = Job::new(d, sha, developer, &job_id)?;
    match pollster::block_on(execute::run(&j, &plan, &mut rec, &mut artifacts, &mut edge)) {
        Ok(o) => {
            // Steps the run went on past (refused before they wrote anything).
            let failed: Vec<String> = rec
                .plan_steps()
                .unwrap_or_default()
                .into_iter()
                .filter(|s| rec.state(s) == StepState::Failed)
                .map(|s| {
                    let why = rec
                        .journal
                        .iter()
                        .rev()
                        .find(|e| e.step == s)
                        .and_then(|e| e.outcome.as_ref())
                        .and_then(|o| o.message.clone())
                        .unwrap_or_default();
                    format!("  {s}: {why}")
                })
                .collect();
            let all = if failed.is_empty() {
                "every step is done".to_string()
            } else {
                format!("the steps are done except these, which wrote nothing:\n{}\n", failed.join("\n"))
            };
            match o {
                Outcome::Complete => println!("installed: {all}"),
                Outcome::WaitingForAcceptance { mode, check } => println!(
                    "{all}; the device is restarting into {mode}. The installer can't see it there yet: check it yourself.\n\
                     {check} GlassineOS sets itself up at its first start and restarts once; then it shows its own screen."
                ),
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(s) => {
            eprintln!("{s}");
            eprintln!(
                "the record is {}; glassine-install resume --record {} goes on from it",
                record_path.display(),
                record_path.display()
            );
            Ok(ExitCode::from(1))
        }
    }
}

/// `install --description FILE --release-dir DIR [--backup DIR] [--developer] [--record PATH]
/// [--facts FACTS.json] [--choice K=V]... [--hardware H=yes|no]...`
pub fn install_cmd(args: &[String]) -> Result<ExitCode, String> {
    let o: Opts = opts(
        args,
        &["--developer", "--pc"],
        &[
            "--root",
            "--description",
            "--release-dir",
            "--backup",
            "--record",
            "--facts",
            "--choice",
            "--hardware",
            "--job",
            "--free",
        ],
    )?;
    let (Some(desc), Some(rel)) = (o.one("--description"), o.one("--release-dir")) else {
        return Err(format!("install needs --description and --release-dir\n\n{USAGE}"));
    };
    let record_path = PathBuf::from(o.one("--record").map(String::as_str).unwrap_or("install-record.json"));
    if record_path.exists() {
        return Err(format!(
            "{} exists: resume it (glassine-install resume --record {}) or name another --record",
            record_path.display(),
            record_path.display()
        ));
    }
    // Originals a run with no record left behind belong to no install.
    let kept = kept_dir(&record_path);
    if kept.exists() {
        std::fs::remove_dir_all(&kept).map_err(|e| format!("{}: {e}", kept.display()))?;
    }
    let (d, sha) = load_description(Path::new(desc))?;
    let rel = PathBuf::from(rel);
    let artifacts = pollster::block_on(Artifacts::open(DirSource(rel.clone()))).map_err(|e| e.to_string())?;
    let mut input = plan_input(&o)?;
    input.release = Some(Release::parse(&read(&rel.join(glassine_installer::artifacts::MANIFEST))?)?);
    let pc = super::pc_root(&o);
    let facts: Snapshot = match (o.one("--facts"), &pc) {
        (Some(_), Some(_)) => return Err("install --pc reads the PC it runs on; it takes no --facts".into()),
        (Some(f), None) => Snapshot::parse(&read(Path::new(f))?).map_err(|e| format!("{f}: {e}"))?,
        (None, Some(root)) => super::live_pc(root)?,
        // restarted afterwards: the executor needs a fresh start of the boot loader
        (None, None) => super::live(&[&d], true)?.snapshot,
    };
    let refs = [&d];
    let rec = recognize::recognize(&refs, &facts);
    let p = plan::plan(&refs, &facts, &rec, &input);
    print!("{}", plan::render(&p, rec.matched(&refs)));
    match p.status {
        Status::Ready => {}
        Status::DeveloperOnly if input.developer => {}
        _ => {
            eprintln!("this plan isn't run");
            return Ok(ExitCode::from(3));
        }
    }
    let plan_json = serde_json::to_string_pretty(&p).map_err(|e| e.to_string())?;
    let backup = o.one("--backup").map(PathBuf::from);
    if let Some(root) = &pc {
        pc_places(root, &plan_json, &record_path, backup.as_deref())?;
    }
    // The artifacts the plan's steps use, as the release states them.
    let mut roles: Vec<String> = vec![];
    for s in &p.steps {
        let base = s.id.split('.').next().unwrap_or(&s.id);
        let Some(st) = d.steps.iter().find(|x| x.id == base) else { continue };
        for k in ["artifact", "initramfs", "loader"] {
            if let Some(r) = st.params.get(k).and_then(|v| v.as_str())
                && !roles.iter().any(|x| x == r)
            {
                roles.push(r.to_string());
            }
        }
    }
    let arts = roles
        .iter()
        .map(|r| artifacts.artifact(r).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    for a in &arts {
        println!("  {}: {} ({} bytes, sha256 {})", a.role, a.file, a.size, a.sha256);
    }
    println!("Type {CONFIRM:?} to run this plan on the device; anything else stops here.");
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())?;
    if line.trim() != CONFIRM {
        println!("not confirmed: nothing was done");
        return Ok(ExitCode::from(1));
    }
    // On a PC the backup is made by the run's first step, into --backup.
    let b = if pc.is_some() {
        None
    } else {
        backup.as_deref().map(verified_backup).transpose()?
    };
    let rec = Record::new(
        env!("CARGO_PKG_VERSION"),
        &d.id,
        &std::fs::read(desc).map_err(|e| e.to_string())?,
        &plan_json,
        Confirmation {
            text: CONFIRM.into(),
            at: None,
        },
        arts,
        b,
    )?;
    record::save(&mut FileStore(record_path.clone()), &rec)?;
    let inputs = serde_json::json!({
        "description": desc,
        "release_dir": rel,
        "backup": backup,
        "developer": input.developer,
        "pc_root": pc,
    });
    // Written whole and synced, like the record: a resume after a power cut needs it.
    let mut whole = FileStore(inputs_path(&record_path));
    whole.write_temp((serde_json::to_string_pretty(&inputs).unwrap() + "\n").as_bytes())?;
    whole.commit()?;
    match pc {
        Some(root) => execute_pc(&d, sha, input.developer, rec, &rel, backup, &record_path, &root),
        None => execute(&d, sha, input.developer, rec, &rel, backup, &record_path),
    }
}

/// The disk a PC plan writes.
fn pc_target(plan_json: &str) -> Result<u32, String> {
    serde_json::from_str::<serde_json::Value>(plan_json)
        .ok()
        .and_then(|v| v.pointer("/space/disk/disk").and_then(|d| d.as_u64()))
        .map(|d| d as u32)
        .ok_or_else(|| "the plan names no target disk".to_string())
}

/// The record and the backup go on a disk the plan doesn't write, and a
/// plan that backs up has a place for its backup.
fn pc_places(root: &Path, plan_json: &str, record: &Path, backup: Option<&Path>) -> Result<(), String> {
    let target = pc_target(plan_json)?;
    let backs_up = serde_json::from_str::<serde_json::Value>(plan_json)
        .ok()
        .and_then(|v| v.pointer("/backup/choice").and_then(|c| c.as_str()).map(|c| c != "off"))
        .unwrap_or(true);
    if backs_up && backup.is_none() {
        return Err("the plan backs up every disk it writes: name the folder with --backup DIR (on another disk), or choose backup=off".into());
    }
    let places = [Some(("the record", record)), backup.map(|b| ("the backup", b))];
    for (what, p) in places.into_iter().flatten() {
        let dir = if p.is_dir() {
            p.to_path_buf()
        } else {
            p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf()
        };
        let abs = std::fs::canonicalize(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        if pc::disk_of(root, &abs) == Some(target) {
            return Err(format!(
                "{what} ({}) is on disk {target}, the disk the plan writes: put it on another disk",
                p.display()
            ));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
fn execute_pc(
    d: &Description,
    sha: String,
    developer: bool,
    mut rec: Record,
    release: &Path,
    backup: Option<PathBuf>,
    record_path: &Path,
    root: &Path,
) -> Result<ExitCode, String> {
    use glassine_installer::pc_execute::{self, PcJob};
    use glassine_installer_pcwrite::{DirEdge, LinuxMachine};
    let mut artifacts = pollster::block_on(Artifacts::open(DirSource(release.to_path_buf()))).map_err(|e| e.to_string())?;
    let mut cancel = record_path.as_os_str().to_owned();
    cancel.push(".cancel");
    let target = pc_target(&rec.plan)?;
    let mut machine = LinuxMachine::new(root, target)?;
    let mut edge = DirEdge::new(record_path, backup, Some(PathBuf::from(&cancel)));
    eprintln!(
        "to stop after the next step that is safe to stop after, create {}",
        PathBuf::from(cancel).display()
    );
    let job = PcJob {
        description: d,
        description_sha256: sha,
        developer,
    };
    let plan = rec.plan.clone();
    match pollster::block_on(pc_execute::run(&job, &plan, &mut rec, &mut artifacts, &mut machine, &mut edge)) {
        Ok(o) => {
            let check = match o {
                Outcome::WaitingForAcceptance { check, .. } => check,
                Outcome::Complete => String::new(),
            };
            println!("every step is done. {check} GlassineOS sets itself up at its first start.");
            Ok(ExitCode::SUCCESS)
        }
        Err(s) => {
            eprintln!("{s}");
            eprintln!(
                "the record is {}; glassine-install resume --record {} goes on from it",
                record_path.display(),
                record_path.display()
            );
            Ok(ExitCode::from(1))
        }
    }
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
fn execute_pc(_: &Description, _: String, _: bool, _: Record, _: &Path, _: Option<PathBuf>, _: &Path, _: &Path) -> Result<ExitCode, String> {
    Err("installing on the PC this runs on works on Linux only".into())
}

/// `resume --record PATH [--backup DIR]`
pub fn resume_cmd(args: &[String]) -> Result<ExitCode, String> {
    let o = opts(args, &[], &["--record", "--backup"])?;
    let record_path = PathBuf::from(o.one("--record").ok_or(format!("resume needs --record\n\n{USAGE}"))?);
    let rec = record::load(&mut FileStore(record_path.clone()))?.ok_or_else(|| format!("{}: no record", record_path.display()))?;
    let inputs: serde_json::Value = serde_json::from_str(&read(&inputs_path(&record_path))?).map_err(|e| e.to_string())?;
    let s = |k: &str| inputs.get(k).and_then(|v| v.as_str()).map(String::from);
    let desc = s("description").ok_or("the inputs name no description")?;
    let rel = PathBuf::from(s("release_dir").ok_or("the inputs name no release folder")?);
    let backup = o.one("--backup").cloned().or_else(|| s("backup")).map(PathBuf::from);
    let developer = inputs.get("developer").and_then(|v| v.as_bool()).unwrap_or(false);
    let (d, sha) = load_description(Path::new(&desc))?;
    let mut rec = rec;
    if let Some(root) = inputs.get("pc_root").and_then(|v| v.as_str()).map(PathBuf::from) {
        pc_places(&root, &rec.plan, &record_path, backup.as_deref())?;
        return execute_pc(&d, sha, developer, rec, &rel, backup, &record_path, &root);
    }
    if rec.backup.is_none()
        && let Some(b) = &backup
    {
        rec.backup = Some(verified_backup(b)?);
    }
    execute(&d, sha, developer, rec, &rel, backup, &record_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(what: &str) -> TempDir {
            let p = std::env::temp_dir().join(format!("glassine-install-{what}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_record_file_is_replaced_whole() {
        let t = TempDir::new("store");
        let mut s = FileStore(t.0.join("r.json"));
        assert_eq!(s.read().unwrap(), None);
        s.write_temp(b"one").unwrap();
        assert_eq!(s.read().unwrap(), None, "the temporary copy isn't the record");
        s.commit().unwrap();
        assert_eq!(s.read().unwrap().unwrap(), b"one");
        s.write_temp(b"two").unwrap();
        assert_eq!(s.read().unwrap().unwrap(), b"one");
        s.commit().unwrap();
        assert_eq!(s.read().unwrap().unwrap(), b"two");
    }

    #[test]
    fn a_release_folder_reads_only_its_own_files() {
        let t = TempDir::new("release");
        std::fs::write(t.0.join("k.img"), b"kernel").unwrap();
        let mut s = DirSource(t.0.clone());
        assert_eq!(pollster::block_on(s.size("k.img")).unwrap(), 6);
        let mut b = [0u8; 3];
        pollster::block_on(s.read_at("k.img", 3, &mut b)).unwrap();
        assert_eq!(&b, b"nel");
        assert!(pollster::block_on(s.size("../k.img")).is_err());
        assert!(pollster::block_on(s.size("/etc/passwd")).is_err());
    }

    #[test]
    fn a_backup_without_its_manifest_is_not_one() {
        let t = TempDir::new("backup");
        assert!(verified_backup(&t.0).is_err());
        std::fs::write(t.0.join("manifest.json"), b"{}\n").unwrap();
        assert!(verified_backup(&t.0).unwrap_err().contains(USER_AREA_NAME));
        // the user area, gzip: the table's bytes come out of it
        let mut z = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        z.write_all(&(0..4096u32).map(|i| i as u8).collect::<Vec<_>>()).unwrap();
        std::fs::write(t.0.join(format!("{USER_AREA_NAME}.img.gz")), z.finish().unwrap()).unwrap();
        let b = verified_backup(&t.0).unwrap();
        assert_eq!(b.manifest_sha256.unwrap(), sha256_hex(b"{}\n"));
        let d: Description = match validate::load(include_str!("../../devices/onyx-noteair.toml")) {
            Loaded::Valid(d) => *d,
            _ => panic!(),
        };
        let mut e = CliEdge {
            d: &d,
            store: FileStore(t.0.join("r.json")),
            kept: t.0.join("r.json.kept"),
            backup: Some(t.0.clone()),
            cancel_file: t.0.join("cancel"),
            shown: BTreeMap::new(),
        };
        assert_eq!(pollster::block_on(e.backup_bytes(512, 4)).unwrap(), [0, 1, 2, 3]);
        // a step's original, kept whole under its name
        assert_eq!(pollster::block_on(e.kept("patch-boot_a-original")).unwrap(), None);
        pollster::block_on(e.keep("patch-boot_a-original", b"stock")).unwrap();
        assert_eq!(pollster::block_on(e.kept("patch-boot_a-original")).unwrap().unwrap(), b"stock");
        assert!(pollster::block_on(e.keep("../x", b"")).is_err());
        assert!(!e.cancel_requested());
        std::fs::write(t.0.join("cancel"), b"").unwrap();
        assert!(e.cancel_requested());
    }

    #[test]
    fn a_busy_interface_names_adb_kill_server() {
        assert!(open_error(usb::OpenError::Busy("Resource busy".into())).contains("adb kill-server"));
    }
}
