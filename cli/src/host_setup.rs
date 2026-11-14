// SPDX-License-Identifier: GPL-2.0-or-later
//! `glassine-install host-setup [--print]`: what this computer needs once to
//! reach a device's USB modes (core `host_setup`). On Linux it installs the
//! udev rules file and reloads udev (as root), or with --print writes the file
//! to stdout and what to run to stderr. On Windows it lists the USB ids that
//! need the WinUSB driver and how to install it.

use glassine_installer::format1::Description;
use glassine_installer::host_setup::{self, RELOAD, RULES_DIR, RULES_FILE, UsbMode};
use std::io::ErrorKind;
use std::path::Path;
use std::process::ExitCode;

pub fn cmd(args: &[String], descs: &[&Description]) -> Result<ExitCode, String> {
    let print = match args {
        [] => false,
        [a] if a == "--print" => true,
        _ => return Err(format!("host-setup takes only --print\n\n{}", crate::USAGE)),
    };
    let rules = host_setup::udev_rules(descs);
    if print {
        print!("{rules}");
        eprintln!("{}", what_to_run());
        return Ok(ExitCode::SUCCESS);
    }
    if cfg!(windows) {
        println!("{}", windows_steps(&host_setup::usb_modes(descs)));
        return Ok(ExitCode::SUCCESS);
    }
    if !cfg!(target_os = "linux") {
        println!("Nothing to set up on this system: it lets programs open USB devices without a driver or a rule.");
        return Ok(ExitCode::SUCCESS);
    }
    let mut released: Vec<String> = host_setup::usb_modes(descs).into_iter().filter_map(|m| m.release_driver).collect();
    released.dedup();
    let said = install(Path::new(RULES_DIR), &rules, &released, &mut run)?;
    println!("{said}");
    Ok(ExitCode::SUCCESS)
}

/// What to run with the printed file (Linux).
fn what_to_run() -> String {
    let reload: Vec<String> = RELOAD.iter().map(|c| format!("  sudo {}", c.join(" "))).collect();
    format!(
        "Install it as {RULES_DIR}/{RULES_FILE}, then load it and apply it to devices already connected:\n\
         \x20 glassine-install host-setup --print 2>/dev/null | sudo tee {RULES_DIR}/{RULES_FILE} > /dev/null\n{}\n\
         Or let the installer do all of it: sudo glassine-install host-setup",
        reload.join("\n")
    )
}

fn run(argv: &[&str]) -> Result<(), String> {
    let st = std::process::Command::new(argv[0]).args(&argv[1..]).status().map_err(|e| e.to_string())?;
    if st.success() { Ok(()) } else { Err(format!("exit status {st}")) }
}

/// Writes `rules` as `dir`/[`RULES_FILE`] (through a temporary file, so a
/// cut write never leaves half a rules file for udev to read), then runs
/// [`RELOAD`] with `run`. Returns what to tell the person.
/// `released`: the kernel drivers the rules unbind (for the message).
pub fn install(dir: &Path, rules: &str, released: &[String], run: &mut dyn FnMut(&[&str]) -> Result<(), String>) -> Result<String, String> {
    let path = dir.join(RULES_FILE);
    let tmp = dir.join(format!(".{RULES_FILE}.tmp"));
    let write = std::fs::write(&tmp, rules).and_then(|()| std::fs::rename(&tmp, &path));
    if let Err(e) = write {
        let _ = std::fs::remove_file(&tmp);
        return Err(if e.kind() == ErrorKind::PermissionDenied {
            format!(
                "not allowed to write {} ({e}): run it as root: sudo glassine-install host-setup\n\
                 (glassine-install host-setup --print shows the file and what it runs)",
                path.display()
            )
        } else {
            format!("{}: {e}", path.display())
        });
    }
    for c in RELOAD {
        if let Err(e) = run(c) {
            return Err(format!(
                "{} is in place, but `{}` failed ({e}): unplug the device and plug it in again, or restart this computer",
                path.display(),
                c.join(" ")
            ));
        }
    }
    Ok(format!(
        "{} is in place and udev reloaded: the devices' USB modes can be opened without root and ModemManager \
         leaves them alone{}. Unplug the device and plug it in again if it was connected.",
        path.display(),
        if released.is_empty() {
            String::new()
        } else {
            format!("; {} lets go of the modes it takes", released.join(", "))
        }
    ))
}

/// The guided WinUSB step, with the USB ids of every mode.
pub fn windows_steps(modes: &[UsbMode]) -> String {
    let mut s = String::from(
        "On Windows the installer reaches a device's USB mode through the WinUSB driver. Install it once for each \
         USB id below, with the device connected in that mode: start Zadig, choose Options > List All Devices, pick \
         the device whose USB ID is shown, choose WinUSB as the driver and click Install Driver (or Replace Driver).\n\
         No driver package is shipped: Windows installs only signed ones, and the devices don't announce WinUSB \
         themselves.\n",
    );
    for m in modes {
        s.push_str(&format!(
            "  {} {}  {}\n",
            m.vendor_id.to_ascii_uppercase(),
            m.product_id.to_ascii_uppercase(),
            m.modes.join(", ")
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("glassine-host-setup-cli-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn installs_the_file_then_reloads_and_triggers() {
        let d = tmpdir("ok");
        let mut ran: Vec<String> = vec![];
        let said = install(&d, "RULES\n", &["qcserial".into()], &mut |c| {
            ran.push(c.join(" "));
            Ok(())
        })
        .unwrap();
        assert_eq!(std::fs::read_to_string(d.join(RULES_FILE)).unwrap(), "RULES\n");
        assert_eq!(ran, ["udevadm control --reload-rules", "udevadm trigger --subsystem-match=usb"]);
        assert!(said.contains("is in place"));
        assert!(said.contains("qcserial lets go of the modes it takes"), "{said}");
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1, "the temporary file is left");
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_failing_udevadm_says_the_file_is_there_and_what_to_do() {
        let d = tmpdir("udevadm");
        let e = install(&d, "RULES\n", &["qcserial".into()], &mut |c| {
            if c[1] == "trigger" { Err("exit status 1".into()) } else { Ok(()) }
        })
        .unwrap_err();
        assert!(
            e.contains("is in place, but `udevadm trigger --subsystem-match=usb` failed (exit status 1)"),
            "{e}"
        );
        assert!(e.contains("plug it in again"));
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_missing_directory_writes_nothing_and_runs_nothing() {
        let d = tmpdir("missing").join("no-such-dir");
        let e = install(&d, "RULES\n", &["qcserial".into()], &mut |_| panic!("ran udevadm")).unwrap_err();
        assert!(e.contains(RULES_FILE), "{e}");
    }

    /// Not root: the directory isn't writable, nothing runs, and the message
    /// says to use sudo. (As root the directory is writable whatever its
    /// mode, so the case can't be made.)
    #[cfg(unix)]
    #[test]
    fn without_root_it_says_to_run_it_with_sudo() {
        use std::os::unix::fs::PermissionsExt;
        let d = tmpdir("ro");
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o555)).unwrap();
        let root = std::fs::write(d.join("probe"), "").is_ok();
        if !root {
            let e = install(&d, "RULES\n", &["qcserial".into()], &mut |_| panic!("ran udevadm")).unwrap_err();
            assert!(e.contains("run it as root: sudo glassine-install host-setup"), "{e}");
            assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
        }
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn the_windows_steps_list_every_id() {
        let m = |v: &str, p: &str, modes: &[&str]| UsbMode {
            vendor_id: v.into(),
            product_id: p.into(),
            interface: None,
            kinds: vec![],
            modes: modes.iter().map(|x| x.to_string()).collect(),
            release_driver: None,
        };
        let s = windows_steps(&[m("05c6", "9008", &["d:emergency"]), m("18d1", "d00d", &["d:fastboot", "d:fastboot-stock"])]);
        assert!(s.contains("  05C6 9008  d:emergency\n"));
        assert!(s.contains("  18D1 D00D  d:fastboot, d:fastboot-stock\n"));
        assert!(s.contains("Zadig") && s.contains("WinUSB"));
    }
}
