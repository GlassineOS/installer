// SPDX-License-Identifier: GPL-2.0-or-later
//! glassine-device-files on the descriptions this repository holds: the files a
//! device's package installs (spec 002 FR-030), as the core derives them.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// Runs the program on `devices/<name>.toml` into a fresh directory: what it
/// printed and the directory.
fn run(name: &str) -> (String, PathBuf) {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("device-files-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_glassine-device-files"))
        .arg(root().join("devices").join(format!("{name}.toml")))
        .arg(&dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    (String::from_utf8(out.stdout).unwrap(), dir)
}

fn files(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    v.sort();
    v
}

fn same(dir: &Path, file: &str, golden: &str) {
    let got = std::fs::read_to_string(dir.join(file)).unwrap();
    let want = std::fs::read_to_string(root().join(golden)).unwrap();
    assert_eq!(got, want, "{file} differs from {golden}");
}

#[test]
fn the_note_air() {
    let (printed, dir) = run("onyx-noteair");
    assert_eq!(
        printed,
        "device-data.list /usr/share/glassine/device-data.list\n\
         slots.conf /etc/glassine/slots.conf\n\
         deploy.conf.device /usr/share/glassine/deploy.conf.device\n"
    );
    assert_eq!(files(&dir), ["deploy.conf.device", "device-data.list", "slots.conf"]);
    same(&dir, "deploy.conf.device", "core/tests/golden/onyx-noteair.deploy.conf.device");
}

/// A PC's package takes glassine-deploy's device facts from here as the Note Air's does.
#[test]
fn the_pc() {
    let (printed, dir) = run("uefi-pc");
    assert_eq!(printed, "deploy.conf.device /usr/share/glassine/deploy.conf.device\n");
    assert_eq!(files(&dir), ["deploy.conf.device"]);
    same(&dir, "deploy.conf.device", "core/tests/golden/uefi-pc.deploy.conf.device");
}

#[test]
fn usage() {
    let out = Command::new(env!("CARGO_BIN_EXE_glassine-device-files")).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}
