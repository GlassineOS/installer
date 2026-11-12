// SPDX-License-Identifier: GPL-2.0-or-later
//! Pinned copies (FR-027, FR-028; SC-004).

mod common;
use common::*;
use glassine_installer::lock::check;

fn copies() -> Vec<(String, Vec<u8>)> {
    ["onyx-noteair.toml", "uefi-pc.toml"]
        .iter()
        .map(|n| (n.to_string(), text(&format!("devices/{n}")).into_bytes()))
        .collect()
}

fn lock() -> String {
    text("devices/lock.toml")
}

#[test]
fn the_shipped_lock_holds() {
    let c = check(&lock(), "GlassineOS/installer", &copies(), false);
    assert!(c.problems.is_empty(), "{:#?}", c.problems);
    assert_eq!(
        c.unpinned,
        vec![("GlassineOS/bootstrap-onyx-noteair".to_string(), "devices/onyx-noteair.toml".to_string())]
    );
}

#[test]
fn one_changed_byte_fails() {
    let mut c = copies();
    let b = &mut c[0].1;
    let at = b.len() / 2;
    b[at] ^= 0x01;
    let r = check(&lock(), "GlassineOS/installer", &c, false);
    assert!(
        r.problems
            .iter()
            .any(|p| p.path == "copies[0].sha256" && p.message.contains("onyx-noteair.toml")),
        "{:#?}",
        r.problems
    );
}

#[test]
fn an_entry_without_a_copy_fails() {
    let c: Vec<_> = copies().into_iter().filter(|(n, _)| n != "uefi-pc.toml").collect();
    let r = check(&lock(), "GlassineOS/installer", &c, false);
    assert!(r.problems.iter().any(|p| p.message.contains("no copy uefi-pc.toml")), "{:#?}", r.problems);
}

#[test]
fn a_copy_without_an_entry_fails() {
    let mut c = copies();
    c.push(("stray.toml".into(), text("devices/uefi-pc.toml").into_bytes()));
    let r = check(&lock(), "GlassineOS/installer", &c, false);
    assert!(
        r.problems.iter().any(|p| p.message.contains("stray.toml has no lock entry")),
        "{:#?}",
        r.problems
    );
}

#[test]
fn a_copy_that_fails_the_format_fails() {
    let mut c = copies();
    c[1].1 = mutate(&text("devices/uefi-pc.toml"), "generic = true", "generic = \"yes\"").into_bytes();
    let l = mutate(
        &lock(),
        &glassine_installer::lock::sha256_hex(text("devices/uefi-pc.toml").as_bytes()),
        &glassine_installer::lock::sha256_hex(&c[1].1),
    );
    let r = check(&l, "GlassineOS/installer", &c, false);
    assert!(r.problems.iter().any(|p| p.path == "uefi-pc.toml:generic"), "{:#?}", r.problems);
}

#[test]
fn unpinned_fails_once_pins_are_required() {
    let r = check(&lock(), "GlassineOS/installer", &copies(), true);
    assert!(r.problems.iter().any(|p| p.path == "copies[0].version"), "{:#?}", r.problems);
}
