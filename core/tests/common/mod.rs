// SPDX-License-Identifier: GPL-2.0-or-later
#![allow(dead_code)]
pub mod img;
use glassine_installer::facts::Snapshot;
use glassine_installer::format1::Description;
use glassine_installer::validate::{Loaded, load};
use std::path::PathBuf;

pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

pub fn text(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

pub fn noteair() -> String {
    text("devices/onyx-noteair.toml")
}

pub fn pc() -> String {
    text("devices/uefi-pc.toml")
}

pub fn valid(t: &str) -> Description {
    match load(t) {
        Loaded::Valid(d) => *d,
        Loaded::Invalid(ps) => panic!("invalid: {ps:#?}"),
        Loaded::TooNew { format } => panic!("too new: {format}"),
    }
}

pub fn facts(name: &str) -> Snapshot {
    Snapshot::parse(&text(&format!("core/tests/fixtures/facts/{name}.json"))).unwrap()
}

/// A release manifest fixture (002 FR-019a).
pub fn release(name: &str) -> glassine_installer::release::Release {
    glassine_installer::release::Release::parse(&text(&format!("core/tests/fixtures/releases/{name}.json"))).unwrap()
}

/// Replaces `old` (which must occur exactly once) with `new`.
pub fn mutate(t: &str, old: &str, new: &str) -> String {
    assert_eq!(t.matches(old).count(), 1, "{old:?} must occur once");
    t.replacen(old, new, 1)
}

/// A PC description that refines the generic one: the same body with its own
/// id and identity.
pub fn refined_pc(id: &str, vendor: &str, product: &str) -> String {
    let t = pc();
    let t = mutate(&t, "id = \"uefi-pc\"", &format!("id = \"{id}\""));
    let t = mutate(&t, "generic = true", "refines = \"uefi-pc\"");
    mutate(
        &t,
        "[[identity]]\nkey = \"cpu.arch\"",
        &format!(
            "[[identity]]\nkey = \"board.vendor\"\nequals = \"{vendor}\"\nrequired = true\nuntried = \"a test fixture\"\n\n[[identity]]\nkey = \"board.product\"\nequals = \"{product}\"\nrequired = true\nuntried = \"a test fixture\"\n\n[[identity]]\nkey = \"cpu.arch\""
        ),
    )
}

/// Writes `got` to the golden file when UPDATE_GOLDEN is set, else compares.
pub fn golden(rel: &str, got: &str) {
    let p = root().join(rel);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel}: {e} (run with UPDATE_GOLDEN=1)"));
    assert_eq!(want, got, "{rel} differs (run with UPDATE_GOLDEN=1 and review the diff)");
}
