// SPDX-License-Identifier: GPL-2.0-or-later
//! The Note Air's partition layout as releases carry it (`layouts/onyx-noteair.json`,
//! the `partition-layout` role, kind `gpt-recipe`), and a release folder in
//! GlassineOS/images' format (its `scripts/pool-image`, docs/artifacts.md "Releases").
//!
//! The shipped layout is the lab fixture (`core/tests/fixtures/gpt/noteair-ab.recipe.json`,
//! whose bytes `gpt_build.rs` proves against the lab's A/B table) with the new
//! partitions' GUIDs left empty: the installer derives them per unit.

mod common;
use common::ramsim::image;
use common::*;
use glassine_installer::artifacts::{Artifacts, MemorySource};
use glassine_installer::format1::{Description, Step};
use glassine_installer::gpt_build::{Change, Recipe};
use glassine_installer::lock::sha256_hex;
use glassine_installer::plan::{Input, Status, plan};
use glassine_installer::recognize::recognize;
use std::collections::BTreeMap;

const LAYOUT: &str = "layouts/onyx-noteair.json";
const DISK: u64 = 61_071_360 * 512;
const MIB: u64 = 1 << 20;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(root().join("core/tests/fixtures/gpt").join(name)).unwrap()
}

fn shipped() -> Recipe {
    serde_json::from_str(&text(LAYOUT)).unwrap()
}

fn table_step(d: &Description) -> &Step {
    d.steps.iter().find(|s| s.kind == "write-partition-table").unwrap()
}

/// The shipped layout is the proven one: the lab fixture but for the GUIDs.
#[test]
fn the_shipped_layout_is_the_lab_fixture_without_guids() {
    let mut lab: Recipe = serde_json::from_slice(&fixture("noteair-ab.recipe.json")).unwrap();
    let s = shipped();
    assert!(s.new.iter().all(|n| n.guid.is_empty()), "a release's layout leaves the GUIDs to the unit");
    for n in &mut lab.new {
        n.guid.clear();
    }
    assert_eq!(s, lab);
}

/// It matches the description's table step (renames, creates, resize, fixed
/// sizes) on a stock table, as the executor checks it before it writes.
#[test]
fn the_shipped_layout_matches_the_descriptions_table_step() {
    let d = valid(&noteair());
    let b = glassine_installer::ops::partition_table::layout(&fixture("noteair-stock.gpt"), DISK, text(LAYOUT).as_bytes(), table_step(&d))
        .unwrap_or_else(|e| panic!("{}", e.why));
    let added: Vec<(&str, u64)> = b
        .changes
        .iter()
        .filter_map(|c| match c {
            Change::Added { name, size_bytes, .. } => Some((name.as_str(), *size_bytes)),
            _ => None,
        })
        .collect();
    assert_eq!(added, vec![("boot_b", 64 * MIB), ("system_b", 3 << 30), ("vendor_b", 800 * MIB)]);
    // A layout that renames differently is refused against the step.
    let other = text(LAYOUT).replace("\"to\": \"vendor_a\"", "\"to\": \"vendor_x\"");
    let e = glassine_installer::ops::partition_table::layout(&fixture("noteair-stock.gpt"), DISK, other.as_bytes(), table_step(&d)).unwrap_err();
    assert!(e.why.contains("renames"), "{}", e.why);
}

/// Slot A's boot partition comes out of the table step pinned, with the
/// description's slot-flag value, so the stock system stays the one started
/// until the set-slot step.
#[test]
fn boot_a_is_pinned_with_the_descriptions_value() {
    let d = valid(&noteair());
    let flags = d.boot_settings.iter().find(|b| b.kind == "slot-flags").unwrap();
    let pinned = flags.params.get("pinned").and_then(|v| v.as_str()).unwrap();
    let entry = flags.params.get("entry").and_then(|v| v.as_str()).unwrap().replace("{slot}", "a");
    let r = shipped().renames.into_iter().find(|r| r.to == entry).unwrap();
    assert_eq!(r.attributes.as_deref(), Some(pinned));
}

/// What images' `scripts/pool-image` writes: release_format 1, the roles the
/// installer reads plus keys it ignores (target, build, trial, missing, per
/// role where it came from).
fn images_release() -> (MemorySource, BTreeMap<&'static str, (String, Vec<u8>)>) {
    let files: BTreeMap<&str, (String, Vec<u8>)> = BTreeMap::from([
        ("base", ("base/0123456789abcdef.pool.img".to_string(), image(3 * MIB as usize, 51))),
        ("kernel", ("kernel/00000000000000aa.img".to_string(), image(4 * MIB as usize, 52))),
        (
            "ram-environment",
            ("ram-environment/00000000000000bb.img".to_string(), image(2 * MIB as usize, 53)),
        ),
        ("boot-loader", ("boot-loader/abl.elf".to_string(), image(145_592, 54))),
        (
            "partition-layout",
            ("partition-layout/onyx-noteair.json".to_string(), text(LAYOUT).into_bytes()),
        ),
        ("recovery", ("recovery/00000000000000cc.img".to_string(), image(5 * MIB as usize, 55))),
    ]);
    let entry = |role: &str, kind: &str, min: u64, extra: serde_json::Value| {
        let (f, b) = &files[role];
        let mut e = serde_json::json!({"file": f, "sha256": sha256_hex(b), "size": b.len(), "kind": kind, "min_partition_bytes": min});
        e.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        e
    };
    let layout_len = files["partition-layout"].1.len() as u64;
    let manifest = serde_json::json!({
        "release_format": 1,
        "target": "onyx-noteair",
        "build": "20261115-120000",
        "trial": false,
        "trial_reasons": [],
        "missing": [],
        "artifacts": {
            "base": entry("base", "btrfs-pool-image", 1 << 30,
                serde_json::json!({"base": "0123456789abcdef", "received_uuid": "x", "kernel_release": "7.3.0", "layout_boot_commit": "0"})),
            "kernel": entry("kernel", "raw-image", 64 * MIB, serde_json::json!({"bootimg": "00000000000000aa", "kernel_release": "7.3.0"})),
            "ram-environment": entry("ram-environment", "raw-image", 2 * MIB, serde_json::json!({"ramenv": "00000000000000bb"})),
            "boot-loader": entry("boot-loader", "raw-image", MIB,
                serde_json::json!({"url": "https://example.invalid/abl.elf", "ref": "glassine1"})),
            "partition-layout": entry("partition-layout", "gpt-recipe", layout_len,
                serde_json::json!({"url": "https://example.invalid/onyx-noteair.json"})),
            "recovery": entry("recovery", "raw-image", 5 * MIB, serde_json::json!({"recovery": "00000000000000cc"})),
        }
    });
    let mut src = MemorySource {
        origin: "images release".into(),
        files: files.values().cloned().collect(),
    };
    src.files.insert("release.json".into(), serde_json::to_vec_pretty(&manifest).unwrap());
    (src, files)
}

/// A release folder in images' format opens, every role checks out, the
/// layout reads as a recipe, and the plan takes its least sizes.
#[test]
fn a_release_folder_in_images_format_is_accepted() {
    let (src, files) = images_release();
    let mut a = Artifacts::open(src).unwrap();
    assert_eq!(a.roles(), files.keys().map(|r| r.to_string()).collect::<Vec<_>>());
    for role in files.keys() {
        a.verify(role, &mut |_| {}).unwrap_or_else(|e| panic!("{role}: {e}"));
    }
    assert_eq!(a.artifact("partition-layout").unwrap().kind.as_deref(), Some("gpt-recipe"));
    assert_eq!(a.artifact("boot-loader").unwrap().kind.as_deref(), Some("raw-image"));
    let r: Recipe = serde_json::from_slice(&files["partition-layout"].1).unwrap();
    assert_eq!(r, shipped());

    let ds = [valid(&noteair()), valid(&pc())];
    let refs: Vec<&Description> = ds.iter().collect();
    let f = facts("noteair-stock-fastboot");
    let i = Input {
        choices: [("keep-stock".to_string(), "no".to_string())].into(),
        hardware: [("emergency-cable".to_string(), true)].into(),
        developer: true,
        release: Some(a.manifest.clone()),
        ..Default::default()
    };
    let p = plan(&refs, &f, &recognize(&refs, &f), &i);
    assert_eq!(p.status, Status::Ready, "{:?}", p.reasons);
    let m = &p.space.minimums[0];
    assert_eq!((m.partition.as_str(), m.at_least), ("system_b", Some(1 << 30)));
}
