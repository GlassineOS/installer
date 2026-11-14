// SPDX-License-Identifier: GPL-2.0-or-later
//! The host setup (`host_setup`): the rules file in the repository and the
//! screens' record stay what the shipped descriptions make, every USB mode
//! they name is in it, and the one line the screens offer writes exactly that
//! file.

mod common;
use common::*;
use glassine_installer::format1::Description;
use glassine_installer::host_setup::{self, RULES_FILE};

/// The shipped descriptions (devices/*.toml), as the command line embeds them.
fn shipped() -> Vec<Description> {
    let mut names: Vec<_> = std::fs::read_dir(root().join("devices"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .filter(|p| p.file_name().is_some_and(|n| n != "lock.toml"))
        .collect();
    names.sort();
    names.iter().map(|p| valid(&std::fs::read_to_string(p).unwrap())).collect()
}

#[test]
fn the_rules_file_and_the_screens_record_are_the_descriptions() {
    let ds = shipped();
    let refs: Vec<&Description> = ds.iter().collect();
    let rules = host_setup::udev_rules(&refs);
    golden(&format!("host/linux/{RULES_FILE}"), &rules);
    golden(
        "web/fixtures/host-setup.json",
        &(serde_json::to_string_pretty(&host_setup::record(&refs)).unwrap() + "\n"),
    );
}

#[test]
fn every_usb_mode_of_every_description_is_covered() {
    let ds = shipped();
    let refs: Vec<&Description> = ds.iter().collect();
    let rules = host_setup::udev_rules(&refs);
    let mut n = 0;
    for d in &ds {
        for m in d.modes.iter() {
            let Some(u) = &m.usb else { continue };
            n += 1;
            let line = rules
                .lines()
                .find(|l| {
                    l.contains(&format!("ATTR{{idVendor}}==\"{}\", ATTR{{idProduct}}==\"{}\"", u.vendor_id, u.product_id))
                        && u.interface.as_ref().is_none_or(|i| l.contains(&format!(":{}:", i.replace('-', ""))))
                })
                .unwrap_or_else(|| panic!("{}:{} ({}:{}) has no rule", d.id, m.id, u.vendor_id, u.product_id));
            assert!(line.contains("TAG+=\"uaccess\""), "{line}");
            assert!(line.contains("ENV{ID_MM_DEVICE_IGNORE}=\"1\""), "{line}");
            assert!(line.starts_with("ENV{DEVTYPE}==\"usb_device\""), "{line}");
        }
    }
    assert!(n > 0, "no USB mode in the shipped descriptions");
}

/// The emergency download mode's interface is unbound from qcserial for its
/// id only; no other mode lets go of a driver, and no module is blacklisted.
#[test]
fn only_the_emergency_mode_lets_go_of_qcserial() {
    let ds = shipped();
    let refs: Vec<&Description> = ds.iter().collect();
    let rules = host_setup::udev_rules(&refs);
    let unbinds: Vec<&str> = rules.lines().filter(|l| l.contains("unbind")).filter(|l| !l.starts_with('#')).collect();
    let emergency: Vec<_> = ds
        .iter()
        .flat_map(|d| &d.modes)
        .filter(|m| m.kind == "emergency-download")
        .filter_map(|m| m.usb.as_ref())
        .collect();
    assert!(!emergency.is_empty());
    assert_eq!(unbinds.len(), 1, "{unbinds:#?}");
    let u = emergency[0];
    assert_eq!(
        unbinds[0],
        format!(
            "ENV{{DEVTYPE}}==\"usb_interface\", DRIVER==\"qcserial\", ATTRS{{idVendor}}==\"{}\", ATTRS{{idProduct}}==\"{}\", RUN+=\"/bin/sh -c 'echo %k > /sys/bus/usb/drivers/qcserial/unbind'\"",
            u.vendor_id, u.product_id
        )
    );
    assert!(!rules.lines().filter(|l| !l.starts_with('#')).any(|l| l.contains("blacklist")));
    // Every rule line is inside the skip for remove events and other subsystems.
    let body: Vec<&str> = rules.lines().filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    assert_eq!(body[0], "ACTION==\"remove\", GOTO=\"glassine_installer_end\"");
    assert_eq!(body[1], "SUBSYSTEM!=\"usb\", GOTO=\"glassine_installer_end\"");
    assert_eq!(*body.last().unwrap(), "LABEL=\"glassine_installer_end\"");
}

/// The line the screens offer, run by a shell with sudo, tee and udevadm
/// standing in: it writes exactly the rules file to the rules directory and
/// then reloads and re-triggers udev, in that order.
#[cfg(unix)]
#[test]
fn the_one_line_writes_the_file_and_reloads() {
    use std::os::unix::fs::PermissionsExt;
    let ds = shipped();
    let refs: Vec<&Description> = ds.iter().collect();
    let rules = host_setup::udev_rules(&refs);
    let line = host_setup::linux_command(&rules);
    assert!(!line.contains('\n'));

    let dir = std::env::temp_dir().join(format!("glassine-host-setup-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let out = dir.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let log = dir.join("log");
    let script = |name: &str, body: &str| {
        let p = bin.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    // sudo runs its command; tee writes into the test's directory instead of /etc.
    script("sudo", "exec \"$@\"");
    script("tee", &format!("echo \"tee $*\" >> {log:?}; cat > {out:?}/\"$(basename \"$1\")\""));
    script("udevadm", &format!("echo \"udevadm $*\" >> {log:?}"));
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
    let st = std::process::Command::new("sh").arg("-c").arg(&line).env("PATH", path).status().unwrap();
    assert!(st.success());
    assert_eq!(std::fs::read_to_string(out.join(RULES_FILE)).unwrap(), rules);
    assert_eq!(
        std::fs::read_to_string(&log).unwrap(),
        format!("tee /etc/udev/rules.d/{RULES_FILE}\nudevadm control --reload-rules\nudevadm trigger --subsystem-match=usb\n")
    );
    std::fs::remove_dir_all(&dir).unwrap();
}
