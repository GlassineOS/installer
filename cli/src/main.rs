// SPDX-License-Identifier: GPL-2.0-or-later
//! glassine-install: the installer's command line, a thin client of the core.
//! All file reading happens here; the core only sees text and bytes.

use glassine_installer::facts::Snapshot;
use glassine_installer::format1::{self, Description};
use glassine_installer::inspect::{self, Host, Inspected};
use glassine_installer::validate::{self, Loaded};
use glassine_installer::{lock, plan, recognize, report};
use glassine_installer_usb as usb;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));

const OWN_REPOSITORY: &str = "GlassineOS/installer";

const USAGE: &str = "usage:
  glassine-install validate FILE... [--lock LOCKFILE [--require-pins]]
  glassine-install inspect [--json] [--out FACTS.json] [--reboot] [--devices DIR]
  glassine-install check [--facts FACTS.json | --reboot] [--json] [--devices DIR]
  glassine-install plan --dry-run [--facts FACTS.json | --reboot] [--choice K=V]... [--hardware H=yes|no]...
                        [--developer] [--free BYTES] [--job JOB] [--json] [--devices DIR]
  glassine-install schema
  glassine-install lock-unpinned LOCKFILE

Descriptions come from the copies built into this program unless --devices names a directory.
inspect reads the one device connected in fastboot mode and writes nothing to it (getvar only;
--reboot restarts it at the end). check and plan read a facts snapshot, or without --facts
inspect the connected device first. A boot loader that only answers in a fresh start of its
mode needs restarting before the next live command: use --reboot, or keep the facts with --out
and pass them with --facts.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("validate") => validate_cmd(&args[1..]),
        Some("inspect") => inspect_cmd(&args[1..]),
        Some("check") => check_cmd(&args[1..]),
        Some("plan") => plan_cmd(&args[1..]),
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
    let (t, ids) = usb::open(&info).map_err(|e| e.to_string())?;
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

/// The snapshot from --facts, or a live inspect without it. `Err(Ok(code))`
/// is a device that couldn't be inspected (already reported).
fn facts(o: &Opts, ds: &[&Description]) -> Result<Result<Snapshot, ExitCode>, String> {
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
    let o = opts(args, &["--json", "--reboot"], &["--out", "--devices"])?;
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
    Ok(input)
}

fn check_cmd(args: &[String]) -> Result<ExitCode, String> {
    let o = opts(args, &["--json", "--reboot"], &["--facts", "--devices"])?;
    let ds = descriptions(&o)?;
    let refs: Vec<&Description> = ds.iter().collect();
    let f = match facts(&o, &refs)? {
        Ok(f) => f,
        Err(code) => return Ok(code),
    };
    let rec = recognize::recognize(&refs, &f);
    let p = plan::plan(&refs, &f, &rec, &plan::Input::default());
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
        &["--json", "--dry-run", "--developer", "--reboot"],
        &["--facts", "--devices", "--choice", "--hardware", "--free", "--job"],
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

fn unpinned_cmd(args: &[String]) -> Result<ExitCode, String> {
    let l = args.first().ok_or(USAGE)?;
    let lock_path = Path::new(l);
    let copies = copies_next_to(lock_path)?;
    for (repo, path) in lock::check(&read(lock_path)?, OWN_REPOSITORY, &named(&copies), false).unpinned {
        println!("{repo} {path}");
    }
    Ok(ExitCode::SUCCESS)
}
