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
use glassine_installer::emergency_read::{self, Expected, Failure, ImageEdge, Options, Restart, Scope};
use glassine_installer::format1::Description;
use glassine_installer::glassine_read::Disk;
use glassine_installer::supplied;
use std::path::Path;
use std::time::Duration;

/// The bound on one transfer: the pause that ends the listening for the
/// programmer's own log lines after it started, the wait for its first
/// answer, and each piece of a read (at most 1 MiB).
pub const TIMEOUT: Duration = Duration::from_secs(10);

const RESTART_BY_HAND: &str = "restart the device by hand: hold the power key ~10 s";

/// The scope the options ask for: `--only-gpt`, `--find-boot-area-sizes`,
/// `--only-boot-areas`, `--partition NAME...`, or the whole disk (the user
/// area and the eMMC boot areas the device has); storing needs `--out`.
pub fn scope(only_gpt: bool, find_sizes: bool, only_boot_areas: bool, partitions: &[String], out: Option<&str>) -> Result<Scope, String> {
    if find_sizes {
        return match (only_gpt, only_boot_areas, partitions.is_empty(), out) {
            (false, false, true, None) => Ok(Scope::BootAreaSizes),
            (_, _, _, Some(_)) => Err("--find-boot-area-sizes stores nothing; it doesn't go with --out".into()),
            _ => Err("--find-boot-area-sizes is a run of its own; it doesn't go with --only-gpt, --only-boot-areas or --partition".into()),
        };
    }
    match (only_gpt, only_boot_areas, partitions.is_empty(), out) {
        (true, true, _, _) => Err("--only-gpt and --only-boot-areas are two different runs; choose one".into()),
        (true, false, false, _) => Err("--only-gpt reads the partition table only; it doesn't go with --partition".into()),
        (true, false, true, _) => Ok(Scope::GptOnly),
        (false, true, false, _) => Err("--only-boot-areas reads the eMMC boot areas only; it doesn't go with --partition".into()),
        (false, _, _, None) => Err("emergency-read stores images: it needs --out DIR (or --only-gpt)".into()),
        (false, true, true, Some(_)) => Ok(Scope::BootAreas),
        (false, false, true, Some(_)) => Ok(Scope::WholeDisk),
        (false, false, false, Some(_)) => Ok(Scope::Partitions(partitions.to_vec())),
    }
}

/// For `--only-gpt` and `--find-boot-area-sizes`: nothing is stored.
struct NoStore;

impl ImageEdge for NoStore {
    fn begin(&mut self, _: &Disk) -> Result<(), String> {
        Err("nothing is stored in this run".into())
    }
    fn write(&mut self, _: &[u8]) -> Result<(), String> {
        Err("nothing is stored in this run".into())
    }
    fn end(&mut self, _: &Disk) -> Result<(), String> {
        Err("nothing is stored in this run".into())
    }
    fn keep(&mut self, _: &Disk) -> Result<String, String> {
        Err("nothing is stored in this run".into())
    }
    fn reopen(&mut self, _: &Disk) -> Result<(), String> {
        Err("nothing is stored in this run".into())
    }
    fn fill(&mut self, _: &mut [u8]) -> Result<usize, String> {
        Err("nothing is stored in this run".into())
    }
    fn incomplete(&mut self, _: &Disk) -> Result<String, String> {
        Err("nothing is stored in this run".into())
    }
    fn manifest(&mut self, _: &str) -> Result<(), String> {
        Err("nothing is stored in this run".into())
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

/// Reads the programmer and checks it against the device's description
/// (004 FR-004), before anything opens the device: a known file goes on; a
/// file that fits but isn't known only with `accept_unknown` (the person's
/// confirmation of the warning); a file known not to work, made for another
/// chip or key where that counts, or not a programmer never does. The
/// refusal says why.
pub fn load(programmer: &Path, d: &Description, file: &str, accept_unknown: bool) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(programmer).map_err(|e| format!("{}: {e}", programmer.display()))?;
    if bytes.is_empty() {
        return Err(format!("{} is empty", programmer.display()));
    }
    let c = supplied::check(d, file, &bytes, None)?;
    let shown = crate::supplied_check::render(&c, &programmer.display().to_string());
    match c.verdict.sendable() {
        Some(false) => Ok(bytes),
        Some(true) if accept_unknown => {
            // The warning, with what the file names, before it is sent.
            let warning = shown.strip_suffix("Nothing was sent anywhere.\n").unwrap_or(&shown);
            eprint!("{warning}");
            eprintln!("Sent because of --accept-unknown.");
            Ok(bytes)
        }
        Some(true) => Err(format!("{shown}Not sent: if you trust where it comes from, add --accept-unknown.")),
        None => Err(format!("{shown}Not sent.")),
    }
}

/// Runs the route with the programmer [`load`] returned, for the device `d`
/// describes (its boot ROM's report is compared with `d` before the
/// programmer is sent); what goes to stdout comes back, the failure as its
/// message.
pub fn run(bytes: &[u8], d: &Description, file: &str, out: Option<&Path>, scope: &Scope) -> Result<String, String> {
    let expected = Expected { description: d, file };
    // Before the device is opened: the route would refuse it too.
    if *scope == Scope::BootAreaSizes && emergency_read::sizes_reported_only(d, file) {
        return Err(format!("--find-boot-area-sizes: {}", emergency_read::NO_SEARCH));
    }
    let mut files = match (scope, out) {
        (Scope::GptOnly | Scope::BootAreaSizes, _) => None,
        (_, Some(dir)) => Some(FileEdge::new(dir)?),
        (_, None) => return Err("no --out DIR".into()),
    };
    let opts = Options {
        listen_first: true,
        resend_configure: true,
        ..Options::default()
    };
    let (mut t, _ids) = crate::emergency::open()?;
    t.set_timeout(TIMEOUT);
    let r = match files.as_mut() {
        Some(f) => pollster::block_on(emergency_read::run_checked(t, bytes, expected, scope, opts, f)),
        None => pollster::block_on(emergency_read::run_checked(t, bytes, expected, scope, opts, &mut NoStore)),
    };
    match r {
        Ok(done) => {
            let mut o = String::new();
            if *scope == Scope::GptOnly {
                o += &format!("partition table ({} sectors of {} bytes):\n", done.disk_size / done.sector as u64, done.sector);
                o += &emergency_read::render_table(&done.table, done.sector);
            }
            if *scope == Scope::BootAreaSizes {
                for z in &done.sizes {
                    o += &format!("{} (physical partition {}): {} bytes, size from {}\n", z.name, z.physical, z.bytes, z.source);
                }
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
        let scope = |g, b, p: &[String], o| scope(g, false, b, p, o);
        assert_eq!(scope(true, false, &[], None), Ok(Scope::GptOnly));
        assert_eq!(scope(true, false, &[], Some("d")), Ok(Scope::GptOnly));
        assert!(scope(true, false, &["devinfo".into()], Some("d")).is_err());
        assert!(scope(false, false, &["devinfo".into()], None).unwrap_err().contains("--out"));
        assert!(scope(false, false, &[], None).is_err());
        assert_eq!(scope(false, false, &[], Some("d")), Ok(Scope::WholeDisk));
        assert_eq!(
            scope(false, false, &["devinfo".into()], Some("d")),
            Ok(Scope::Partitions(vec!["devinfo".into()]))
        );
        // --only-boot-areas: stored, so with --out; alone
        assert_eq!(scope(false, true, &[], Some("d")), Ok(Scope::BootAreas));
        assert!(scope(false, true, &[], None).unwrap_err().contains("--out"));
        assert!(scope(true, true, &[], Some("d")).is_err());
        assert!(scope(false, true, &["devinfo".into()], Some("d")).is_err());
    }

    /// `--find-boot-area-sizes`: read-only, alone, without --out.
    #[test]
    fn the_size_search_is_a_run_of_its_own() {
        assert_eq!(scope(false, true, false, &[], None), Ok(Scope::BootAreaSizes));
        assert!(scope(false, true, false, &[], Some("d")).unwrap_err().contains("--out"));
        assert!(scope(true, true, false, &[], None).is_err());
        assert!(scope(false, true, true, &[], None).is_err());
        assert!(scope(false, true, false, &["devinfo".into()], None).is_err());
    }

    use glassine_installer::validate::{self, Loaded};

    fn load_desc(t: &str) -> Description {
        match validate::load(t) {
            Loaded::Valid(d) => *d,
            _ => panic!("the Note Air's description doesn't load"),
        }
    }

    /// The Note Air's description, its known-bad checksum replaced by `bad`'s.
    fn noteair(bad: &[u8]) -> Description {
        let t = crate::EMBEDDED.iter().find(|(n, _)| n.contains("onyx-noteair")).unwrap().1;
        let sum = supplied::check(&load_desc(t), "emergency-programmer", bad, None).unwrap().sha256;
        load_desc(&t.replace("4c2c494e92551b1d7f06f7beece875c0ef2b9b7b2ffab347f76a3dda8fa3d20c", &sum))
    }

    /// A new directory under the system's temporary one, removed at the end.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(what: &str) -> TempDir {
            let p = std::env::temp_dir().join(format!("glassine-load-{what}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        fn file(&self, name: &str, bytes: &[u8]) -> std::path::PathBuf {
            let p = self.0.join(name);
            std::fs::write(&p, bytes).unwrap();
            p
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// `load` is all that runs before the device is opened (`emergency_read_cmd`):
    /// a file it refuses never reaches a device.
    #[test]
    fn a_missing_programmer_touches_no_device() {
        let d = noteair(b"x");
        let e = load(Path::new("/nonexistent/FILE"), &d, "emergency-programmer", true).unwrap_err();
        assert!(e.starts_with("/nonexistent/FILE: "), "{e}");
    }

    /// 004 FR-004: a file known not to work, or not a programmer, is refused
    /// with why, whatever the flags; the description's reason is named.
    #[test]
    fn a_file_that_is_never_sent_is_refused_before_the_device() {
        let dir = TempDir::new("refused");
        let d = noteair(b"the loader that hangs\n");
        let bad = dir.file("bad.elf", b"the loader that hangs\n");
        for accept in [false, true] {
            let e = load(&bad, &d, "emergency-programmer", accept).unwrap_err();
            assert!(e.contains("known not to work") && e.contains("partition table"), "{e}");
            assert!(e.ends_with("Not sent."), "{e}");
        }
        let junk = dir.file("junk.bin", b"hello, world\n");
        let e = load(&junk, &d, "emergency-programmer", true).unwrap_err();
        assert!(e.contains("not a program for this route"), "{e}");
        assert!(load(&junk, &d, "nope", true).is_err());
    }

    /// The synthetic programmer files of the core's tests (web/fixtures/supplied/).
    fn fixture(name: &str) -> Vec<u8> {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("../web/fixtures/supplied/{name}.hex"));
        let h: String = std::fs::read_to_string(p).unwrap().split_whitespace().collect();
        (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect()
    }

    /// A file that fits but isn't known is sent only with --accept-unknown;
    /// a known one without it.
    #[test]
    fn an_unknown_file_needs_the_persons_confirmation() {
        let dir = TempDir::new("unknown");
        let d = noteair(b"x");
        let fits = fixture("fits-unknown");
        let p = dir.file("fits.elf", &fits);
        let e = load(&p, &d, "emergency-programmer", false).unwrap_err();
        assert!(e.contains("not known") && e.ends_with("add --accept-unknown."), "{e}");
        assert_eq!(load(&p, &d, "emergency-programmer", true).unwrap(), fits);
        let known = fixture("known");
        let sum = supplied::check(&d, "emergency-programmer", &known, None).unwrap().sha256;
        let t = crate::EMBEDDED.iter().find(|(n, _)| n.contains("onyx-noteair")).unwrap().1;
        let d = load_desc(&t.replace("b3e409558677f9b280fed7851e8cc66f0079ad78ef0b07519a3a98d86c14d41b", &sum));
        let p = dir.file("known.elf", &known);
        assert_eq!(load(&p, &d, "emergency-programmer", false).unwrap(), known);
    }
}
