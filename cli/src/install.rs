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
//! plan, description or artifact).

use super::{Opts, USAGE, opts, plan_input, read};
use flate2::read::GzDecoder;
use glassine_installer::artifacts::{ArtifactSource, Artifacts};
use glassine_installer::emergency_read::USER_AREA_NAME;
use glassine_installer::execute::{self, InstallEdge, Job, Link, Outcome, Reach};
use glassine_installer::facts::Snapshot;
use glassine_installer::format1::{Description, Mode, Step};
use glassine_installer::lock::sha256_hex;
use glassine_installer::plan::{self, Status};
use glassine_installer::record::{self, Backup, Confirmation, Record, RecordStore};
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
    fn size(&mut self, file: &str) -> Result<u64, String> {
        let p = self.path(file)?;
        std::fs::metadata(&p).map(|m| m.len()).map_err(|e| format!("{}: {e}", p.display()))
    }
    fn read_at(&mut self, file: &str, offset: u64, buf: &mut [u8]) -> Result<(), String> {
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

/// True when `adb devices` lists one device that is on and authorised.
fn adb_sees_device() -> bool {
    let Some(a) = adb() else { return false };
    match Command::new(a).arg("devices").output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).lines().skip(1).any(|l| l.trim_end().ends_with("\tdevice")),
        Err(_) => false,
    }
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
    fn try_fastboot(&self) -> Result<Option<usb::UsbTransport>, String> {
        match usb::find_kind(usb::Kind::Fastboot) {
            Ok(info) => usb::open_kind(&info, usb::Kind::Fastboot).map(|(t, _)| Some(t)).map_err(open_error),
            Err(usb::OpenError::NoDevice) => Ok(None),
            Err(e) => Err(open_error(e)),
        }
    }
    fn try_ram(&self) -> Result<Option<usb::UsbTransport>, String> {
        match usb::find_kind(usb::Kind::RamRead) {
            Ok(info) => usb::open_kind(&info, usb::Kind::RamRead)
                .map(|(mut t, _)| {
                    t.set_timeout(RAM_TRANSFER_TIMEOUT);
                    Some(t)
                })
                .map_err(open_error),
            Err(usb::OpenError::NoDevice) => Ok(None),
            Err(e) => Err(open_error(e)),
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
        loop {
            if (any || mode.kind == "fastboot")
                && let Some(t) = self.try_fastboot()?
            {
                return Ok(Link::Fastboot(t));
            }
            if (any || mode.kind == glassine_installer::ram_backup::RAM_MODE_KIND)
                && let Some(t) = self.try_ram()?
            {
                return Ok(Link::Ram(t));
            }
            if any
                && adb_sees_device()
                && let Some(m) = self.d.modes.iter().find(|m| m.kind == "android-adb")
            {
                return Ok(Link::Elsewhere(m.id.clone()));
            }
            if Instant::now() >= deadline {
                return Err(format!("the device didn't show up in {} within {} s", mode.id, REACH_WAIT.as_secs()));
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    fn save(&mut self, r: &Record) -> Result<(), String> {
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

    fn backup_bytes(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, String> {
        let dir = self.backup.as_ref().ok_or("no backup folder")?;
        let p = dir.join(format!("{USER_AREA_NAME}.img.gz"));
        let f = File::open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let mut z = GzDecoder::new(std::io::BufReader::new(f));
        std::io::copy(&mut (&mut z).take(offset), &mut std::io::sink()).map_err(|e| format!("{}: {e}", p.display()))?;
        let mut b = vec![0u8; len];
        z.read_exact(&mut b).map_err(|e| format!("{}: {e}", p.display()))?;
        Ok(b)
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

fn job(d: &Description, sha: String, developer: bool) -> Job<'_> {
    // TODO(write_rules.rs): the rules derived from the description's modes;
    // until then the executor's minimal ones (ops::minimal_write_rules).
    Job {
        description: d,
        description_sha256: sha,
        developer,
        write_rules: BTreeMap::new(),
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
    let mut artifacts = Artifacts::open(DirSource(release.to_path_buf())).map_err(|e| e.to_string())?;
    let mut cancel = record_path.as_os_str().to_owned();
    cancel.push(".cancel");
    let mut edge = CliEdge {
        d,
        store: FileStore(record_path.to_path_buf()),
        backup,
        cancel_file: PathBuf::from(cancel),
        shown: BTreeMap::new(),
    };
    eprintln!("to stop after the next step that is safe to stop after, create {}", edge.cancel_file.display());
    let plan = rec.plan.clone();
    let j = job(d, sha, developer);
    match pollster::block_on(execute::run(&j, &plan, &mut rec, &mut artifacts, &mut edge)) {
        Ok(Outcome::Complete) => {
            println!("installed: every step is done");
            Ok(ExitCode::SUCCESS)
        }
        Ok(Outcome::WaitingForAcceptance { mode, check }) => {
            println!(
                "every step is done and the device is restarting into {mode}. The installer can't see it there yet: check it yourself.\n\
                 {check} GlassineOS sets itself up at its first start and restarts once; then it shows its own screen."
            );
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
        &["--developer"],
        &[
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
    let (d, sha) = load_description(Path::new(desc))?;
    let rel = PathBuf::from(rel);
    let artifacts = Artifacts::open(DirSource(rel.clone())).map_err(|e| e.to_string())?;
    let mut input = plan_input(&o)?;
    input.release = Some(Release::parse(&read(&rel.join(glassine_installer::artifacts::MANIFEST))?)?);
    let facts: Snapshot = match o.one("--facts") {
        Some(f) => Snapshot::parse(&read(Path::new(f))?).map_err(|e| format!("{f}: {e}"))?,
        // restarted afterwards: the executor needs a fresh start of the boot loader
        None => super::live(&[&d], true)?.snapshot,
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
    // The artifacts the plan's steps use, as the release states them.
    let mut roles: Vec<String> = vec![];
    for s in &p.steps {
        if let Some(r) = d
            .steps
            .iter()
            .find(|x| x.id == s.id)
            .and_then(|x| x.params.get("artifact"))
            .and_then(|v| v.as_str())
            && !roles.iter().any(|x| x == r)
        {
            roles.push(r.to_string());
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
    let backup = o.one("--backup").map(PathBuf::from);
    let b = backup.as_deref().map(verified_backup).transpose()?;
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
    });
    std::fs::write(inputs_path(&record_path), serde_json::to_string_pretty(&inputs).unwrap() + "\n").map_err(|e| e.to_string())?;
    execute(&d, sha, input.developer, rec, &rel, backup, &record_path)
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
        assert_eq!(s.size("k.img").unwrap(), 6);
        let mut b = [0u8; 3];
        s.read_at("k.img", 3, &mut b).unwrap();
        assert_eq!(&b, b"nel");
        assert!(s.size("../k.img").is_err());
        assert!(s.size("/etc/passwd").is_err());
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
            backup: Some(t.0.clone()),
            cancel_file: t.0.join("cancel"),
            shown: BTreeMap::new(),
        };
        assert_eq!(e.backup_bytes(512, 4).unwrap(), [0, 1, 2, 3]);
        assert!(!e.cancel_requested());
        std::fs::write(t.0.join("cancel"), b"").unwrap();
        assert!(e.cancel_requested());
    }

    #[test]
    fn a_busy_interface_names_adb_kill_server() {
        assert!(open_error(usb::OpenError::Busy("Resource busy".into())).contains("adb kill-server"));
    }
}
