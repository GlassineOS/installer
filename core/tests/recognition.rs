// SPDX-License-Identifier: GPL-2.0-or-later
//! Recognition tables (FR-007, FR-007a, FR-009; 002 Scenario 3).

mod common;
use common::*;
use glassine_installer::format1::Description;
use glassine_installer::recognize::{Outcome, StateResult, recognize};

fn shipped() -> Vec<Description> {
    vec![valid(&noteair()), valid(&pc())]
}

fn outcome(ds: &[Description], facts_name: &str) -> Outcome {
    let refs: Vec<&Description> = ds.iter().collect();
    recognize(&refs, &facts(facts_name)).outcome
}

#[test]
fn stock_note_air_in_fastboot() {
    match outcome(&shipped(), "noteair-stock-fastboot") {
        Outcome::Match { id, state } => {
            assert_eq!(id, "onyx-noteair");
            assert_eq!(state, StateResult::Decided { state: "stock".into() });
        }
        o => panic!("{o:?}"),
    }
}

/// The GlassineOS boot loader answers product=NoteAir: the device is
/// recognised, but fastboot can't tell prepared from installed.
#[test]
fn prepared_note_air_needs_glassine_to_tell_the_state() {
    match outcome(&shipped(), "noteair-prepared-fastboot") {
        Outcome::Match {
            id,
            state: StateResult::Undetermined { holding, needs_modes, .. },
        } => {
            assert_eq!(id, "onyx-noteair");
            assert_eq!(holding, vec!["prepared", "installed"]);
            assert_eq!(needs_modes, vec!["glassine"]);
        }
        o => panic!("{o:?}"),
    }
}

#[test]
fn stock_note_air_over_adb_needs_fastboot() {
    match outcome(&shipped(), "noteair-stock-adb") {
        Outcome::Undetermined { candidates, needs_modes } => {
            assert_eq!(candidates, vec!["onyx-noteair"]);
            assert!(
                needs_modes.contains(&"fastboot".to_string()) && needs_modes.contains(&"fastboot-stock".to_string()),
                "{needs_modes:?}"
            );
        }
        o => panic!("{o:?}"),
    }
}

/// The RAM environment the backup runs in reads the storage but can't tell a
/// state: it must never call a stock device prepared.
#[test]
fn stock_note_air_in_the_ram_environment_has_no_state() {
    let ds = shipped();
    let refs: Vec<&Description> = ds.iter().collect();
    let mut f = facts("noteair-stock-adb");
    f.mode = "glassine-ram".into();
    f.android = None;
    f.glassine = Some(glassine_installer::facts::GlassineFacts { installation: false });
    match recognize(&refs, &f).outcome {
        Outcome::Undetermined { candidates, needs_modes } => {
            assert_eq!(candidates, vec!["onyx-noteair"]);
            assert!(needs_modes.iter().any(|m| m.starts_with("fastboot")), "{needs_modes:?}");
        }
        Outcome::Match { state, .. } => assert!(matches!(state, StateResult::Undetermined { .. }), "{state:?}"),
        o => panic!("{o:?}"),
    }
}

#[test]
fn conflicting_signals_are_unsupported_and_named() {
    let ds = shipped();
    let refs: Vec<&Description> = ds.iter().collect();
    let r = recognize(&refs, &facts("noteair-conflict-fastboot"));
    assert!(
        matches!(&r.outcome, Outcome::Conflict { ids } if ids == &vec!["onyx-noteair".to_string()]),
        "{:?}",
        r.outcome
    );
    let na = r.descriptions.iter().find(|d| d.id == "onyx-noteair").unwrap();
    assert!(
        na.signals
            .iter()
            .any(|s| s.key == "storage.partition.onyxconfig.size" && format!("{:?}", s.result) == "False")
    );
}

#[test]
fn unknown_device_is_none() {
    assert!(matches!(outcome(&shipped(), "unknown-fastboot"), Outcome::None));
}

#[test]
fn generic_pc() {
    match outcome(&shipped(), "pc-windows") {
        Outcome::Match { id, state } => {
            assert_eq!(id, "uefi-pc");
            assert_eq!(state, StateResult::Decided { state: "stock".into() });
        }
        o => panic!("{o:?}"),
    }
}

#[test]
fn a_refining_description_wins_over_the_generic_one() {
    let mut ds = shipped();
    ds.push(valid(&refined_pc("example-laptop", "Example Computers", "Example 13")));
    assert!(matches!(outcome(&ds, "pc-windows"), Outcome::Match { id, .. } if id == "example-laptop"));
}

/// Another model of the same maker half-matches the refinement: it is just
/// not that model, and the generic description holds.
#[test]
fn a_half_matching_refinement_falls_back_to_what_it_refines() {
    let mut ds = shipped();
    ds.push(valid(&refined_pc("example-laptop", "Example Computers", "Example 13")));
    assert!(matches!(outcome(&ds, "pc-windows-other-model"), Outcome::Match { id, .. } if id == "uefi-pc"));
}

#[test]
fn two_matches_neither_refining_the_other_are_ambiguous() {
    let mut ds = shipped();
    ds.push(valid(&refined_pc("example-laptop", "Example Computers", "Example 13")));
    ds.push(valid(&refined_pc("example-laptop-too", "Example Computers", "Example 13")));
    match outcome(&ds, "pc-windows") {
        Outcome::Ambiguous { ids } => assert_eq!(ids, vec!["example-laptop", "example-laptop-too"]),
        o => panic!("{o:?}"),
    }
}

#[test]
fn installed_pc_state() {
    let ds = shipped();
    let refs: Vec<&Description> = ds.iter().collect();
    let mut f = facts("pc-windows");
    f.glassine.as_mut().unwrap().installation = true;
    f.storage.as_mut().unwrap().disks[0].partitions.push(glassine_installer::facts::Partition {
        index: 5,
        name: Some("glassine-system".into()),
        size: 1 << 30,
        ..Default::default()
    });
    assert!(matches!(recognize(&refs, &f).outcome, Outcome::Match { state: StateResult::Decided { state }, .. } if state == "installed"));
}

#[test]
fn no_device_is_named_in_the_code() {
    // SC-001: the core reads device knowledge from descriptions only.
    for f in [
        "format1.rs",
        "facts.rs",
        "lock.rs",
        "plan.rs",
        "recognize.rs",
        "report.rs",
        "shape.rs",
        "validate.rs",
        "vocab.rs",
    ] {
        let src = text(&format!("core/src/{f}"));
        for name in ["noteair", "NoteAir", "onyx", "sdm660", "Note Air"] {
            assert!(!src.contains(name), "core/src/{f} names {name}");
        }
    }
}

/// Recorded from a real Note Air on the GlassineOS boot loader (A/B layout,
/// slot b) by `glassine-install inspect`, unchanged: the boot loader pads
/// partition sizes with a leading space. Same result as the hand-made fixture.
#[test]
fn recorded_note_air_on_glassine_boot_loader() {
    match outcome(&shipped(), "noteair-recorded-glassine-fastboot") {
        Outcome::Match {
            id,
            state: StateResult::Undetermined { holding, needs_modes, .. },
        } => {
            assert_eq!(id, "onyx-noteair");
            assert_eq!(holding, vec!["prepared", "installed"]);
            assert_eq!(needs_modes, vec!["glassine"]);
        }
        o => panic!("{o:?}"),
    }
}
