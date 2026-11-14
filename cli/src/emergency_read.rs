// SPDX-License-Identifier: GPL-2.0-or-later
//! `emergency-read --programmer FILE`: reads the storage of the one device
//! in its Qualcomm emergency download mode (USB 05c6:9008) through the
//! programmer the person supplies (spec 004), then restarts it. The core
//! runs the route ([`emergency_read::run`]); this edge opens the device,
//! reads the file the person named and stores the images.
//!
//! The programmer comes only from `--programmer`: nothing here looks for
//! one, downloads one or sends it anywhere but to the device (FR-003).
//! `GLASSINE_USB_TRACE=1` logs every bulk transfer (the USB edge does it).

use crate::backup::FileEdge;
use glassine_installer::emergency_read::{self, Failure, ImageEdge, Restart, Scope, Sizes};
use glassine_installer::glassine_read::Disk;
use std::path::Path;
use std::time::Duration;

/// The bound on one transfer: the programmer's first answer, and each piece
/// of a read (at most 1 MiB).
pub const TIMEOUT: Duration = Duration::from_secs(30);

const RESTART_BY_HAND: &str = "restart the device by hand: hold the power key ~10 s";

/// The scope the options ask for: `--only-gpt`, `--partition NAME...`, or
/// the whole user area; storing needs `--out`.
pub fn scope(only_gpt: bool, partitions: &[String], out: Option<&str>) -> Result<Scope, String> {
    match (only_gpt, partitions.is_empty(), out) {
        (true, false, _) => Err("--only-gpt reads the partition table only; it doesn't go with --partition".into()),
        (true, true, _) => Ok(Scope::GptOnly),
        (false, _, None) => Err("emergency-read stores images: it needs --out DIR (or --only-gpt)".into()),
        (false, true, Some(_)) => Ok(Scope::WholeDisk),
        (false, false, Some(_)) => Ok(Scope::Partitions(partitions.to_vec())),
    }
}

/// For `--only-gpt`: nothing is stored.
struct NoStore;

impl ImageEdge for NoStore {
    fn begin(&mut self, _: &Disk) -> Result<(), String> {
        Err("nothing is stored with --only-gpt".into())
    }
    fn write(&mut self, _: &[u8]) -> Result<(), String> {
        Err("nothing is stored with --only-gpt".into())
    }
    fn end(&mut self, _: &Disk) -> Result<String, String> {
        Err("nothing is stored with --only-gpt".into())
    }
    fn reopen(&mut self, _: &Disk) -> Result<(), String> {
        Err("nothing is stored with --only-gpt".into())
    }
    fn fill(&mut self, _: &mut [u8]) -> Result<usize, String> {
        Err("nothing is stored with --only-gpt".into())
    }
    fn incomplete(&mut self, _: &Disk) -> Result<String, String> {
        Err("nothing is stored with --only-gpt".into())
    }
    fn manifest(&mut self, _: &str) -> Result<(), String> {
        Err("nothing is stored with --only-gpt".into())
    }
    fn progress(&mut self, _: &Disk, _: u64) {}
    fn note(&mut self, line: &str) {
        eprintln!("{line}");
    }
}

fn restart_line(r: &Restart) -> String {
    match r {
        Restart::Done => "the device is restarting into its own system".into(),
        Restart::Failed(e) => format!("the restart wasn't confirmed ({e}); if the screen stays dark, {RESTART_BY_HAND}"),
        Restart::NotPossible => format!("nothing on the device answers that could restart it; {RESTART_BY_HAND}"),
    }
}

/// Runs the route; what goes to stdout comes back, the failure as its
/// message.
pub fn run(programmer: &Path, out: Option<&Path>, scope: &Scope) -> Result<String, String> {
    // Read before the device is touched: a missing file sends nothing.
    let bytes = std::fs::read(programmer).map_err(|e| format!("{}: {e}", programmer.display()))?;
    if bytes.is_empty() {
        return Err(format!("{} is empty", programmer.display()));
    }
    let mut files = match (scope, out) {
        (Scope::GptOnly, _) => None,
        (_, Some(dir)) => Some(FileEdge::new(dir)?),
        (_, None) => return Err("no --out DIR".into()),
    };
    let (mut t, _ids) = crate::emergency::open()?;
    t.set_timeout(TIMEOUT);
    let r = match files.as_mut() {
        Some(f) => pollster::block_on(emergency_read::run(t, &bytes, scope, Sizes::default(), f)),
        None => pollster::block_on(emergency_read::run(t, &bytes, scope, Sizes::default(), &mut NoStore)),
    };
    match r {
        Ok(done) => {
            let mut o = String::new();
            if *scope == Scope::GptOnly {
                o += &format!("partition table ({} sectors of {} bytes):\n", done.disk_size / done.sector as u64, done.sector);
                o += &emergency_read::render_table(&done.table, done.sector);
            }
            for (rec, _, file) in &done.images {
                o += &format!("{file}: {} bytes, sha256 {}, read twice and checked\n", rec.disk.size, rec.sha256_hex());
            }
            if done.manifest.is_some() {
                o += "manifest.json\n";
            }
            eprintln!("{}", restart_line(&done.restart));
            Ok(o)
        }
        Err(Failure { error, restart, manifest }) => {
            let mut e = error.to_string();
            if manifest.is_some() {
                e += "\nmanifest.json lists the images that are complete and those that aren't";
            }
            e += &format!("\n{}", restart_line(&restart));
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options() {
        assert_eq!(scope(true, &[], None), Ok(Scope::GptOnly));
        assert_eq!(scope(true, &[], Some("d")), Ok(Scope::GptOnly));
        assert!(scope(true, &["devinfo".into()], Some("d")).is_err());
        assert!(scope(false, &["devinfo".into()], None).unwrap_err().contains("--out"));
        assert!(scope(false, &[], None).is_err());
        assert_eq!(scope(false, &[], Some("d")), Ok(Scope::WholeDisk));
        assert_eq!(scope(false, &["devinfo".into()], Some("d")), Ok(Scope::Partitions(vec!["devinfo".into()])));
    }

    #[test]
    fn a_missing_programmer_touches_no_device() {
        let e = run(Path::new("/nonexistent/FILE"), None, &Scope::GptOnly).unwrap_err();
        assert!(e.starts_with("/nonexistent/FILE: "), "{e}");
    }
}
