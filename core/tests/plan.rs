// SPDX-License-Identifier: GPL-2.0-or-later
//! Plan golden files (001 FR-003, FR-005; 002 FR-014a, FR-023, Scenario 4),
//! and SC-003's escape walk.

mod common;
use common::*;
use glassine_installer::format1::Description;
use glassine_installer::plan::{Input, Status, plan, render};
use glassine_installer::recognize::recognize;

fn run(name: &str, facts_name: &str, input: Input) -> glassine_installer::plan::Plan {
    let ds = [valid(&noteair()), valid(&pc())];
    let refs: Vec<&Description> = ds.iter().collect();
    let f = facts(facts_name);
    let rec = recognize(&refs, &f);
    let p = plan(&refs, &f, &rec, &input);
    golden(&format!("core/tests/golden/{name}.json"), &(serde_json::to_string_pretty(&p).unwrap() + "\n"));
    golden(&format!("core/tests/golden/{name}.txt"), &render(&p, rec.matched(&refs)));
    p
}

fn input(choices: &[(&str, &str)], hardware: &[(&str, bool)], developer: bool) -> Input {
    Input {
        choices: choices.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        hardware: hardware.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        developer,
        ..Default::default()
    }
}

#[test]
fn note_air_keep_stock() {
    let p = run("noteair-keep-stock", "noteair-stock-fastboot", Input::default());
    assert_eq!(p.status, Status::NeedsAnswers);
    assert_eq!(p.questions[0].hardware, "emergency-cable");
    assert!(p.steps.iter().all(|s| s.id != "retire-stock"));
    let userdata = p.partitions.iter().find(|x| x.partition == "userdata").unwrap();
    assert!(userdata.loses_files);
    assert!(!p.space.exact, "artifact sizes are unknown, so the space is 'at least'");
}

#[test]
fn note_air_without_stock_for_a_developer() {
    let p = run(
        "noteair-no-stock-developer",
        "noteair-stock-fastboot",
        input(&[("keep-stock", "no")], &[("emergency-cable", true)], true),
    );
    assert_eq!(p.status, Status::Ready);
    assert!(p.steps.iter().any(|s| s.id == "retire-stock"));
}

#[test]
fn untried_write_steps_are_for_developers_only() {
    let p = run("noteair-cable-yes", "noteair-stock-fastboot", input(&[], &[("emergency-cable", true)], false));
    assert_eq!(p.status, Status::DeveloperOnly);
    assert!(p.untried_steps.contains(&"replace-boot-loader".to_string()));
}

#[test]
fn without_the_cable_the_device_is_refused() {
    let p = run("noteair-cable-no", "noteair-stock-fastboot", input(&[], &[("emergency-cable", false)], true));
    assert_eq!(p.status, Status::Refused);
}

#[test]
fn not_enough_space_is_refused_at_the_plan() {
    let ds = [valid(&noteair())];
    let refs: Vec<&Description> = ds.iter().collect();
    let f = facts("noteair-stock-fastboot");
    let p = plan(
        &refs,
        &f,
        &recognize(&refs, &f),
        &Input {
            free_bytes: Some(1 << 20),
            developer: true,
            ..Default::default()
        },
    );
    assert_eq!(p.status, Status::Refused);
    assert!(p.reasons.iter().any(|r| r.contains("not enough space")));
}

#[test]
fn pc_with_windows_kept() {
    let p = run("pc-windows-keep", "pc-windows", Input::default());
    assert_eq!(p.status, Status::DeveloperOnly);
    assert!(p.steps.iter().all(|s| !s.id.starts_with("remove-system")));
    assert_eq!(p.systems[0].outcome, "kept and still startable");
}

#[test]
fn pc_with_windows_replaced() {
    let p = run("pc-windows-replace", "pc-windows", input(&[("systems.1", "replace")], &[], true));
    assert_eq!(p.status, Status::Ready);
    assert!(p.steps.iter().any(|s| s.id == "remove-system.1"));
    assert!(p.partitions.iter().filter(|x| x.loses_files).count() == 3);
    assert_eq!(p.systems[0].outcome, "replaced");
}

#[test]
fn a_prepared_device_in_fastboot_is_not_planned_for() {
    let ds = [valid(&noteair())];
    let refs: Vec<&Description> = ds.iter().collect();
    let f = facts("noteair-prepared-fastboot");
    let p = plan(&refs, &f, &recognize(&refs, &f), &Input::default());
    assert_eq!(p.status, Status::Refused);
    assert!(p.reasons[0].contains("glassine"));
}

/// SC-003 (the part that needs no device): for every step of both
/// descriptions, an interruption right after it leaves one of 001's three end
/// states, reached only through what the description names.
#[test]
fn every_interruption_ends_in_one_of_three_states() {
    for d in [valid(&noteair()), valid(&pc())] {
        for s in &d.steps {
            let e = &s.escape.interruption;
            assert!(
                ["previous-system-starts", "glassine-starts", "installer-can-finish", "installer-can-undo"].contains(&e.leaves.as_str()),
                "{}: {} leaves {}",
                d.id,
                s.id,
                e.leaves
            );
            match (&e.way_out.step, &e.way_out.transition) {
                (Some(st), None) => assert!(d.step(st).is_some(), "{}: {} -> step {st}", d.id, s.id),
                (None, Some(t)) => {
                    let t = d.transition(t).unwrap();
                    assert!(
                        t.needs.iter().all(|n| n != "emergency-cable"),
                        "{}: {} needs special hardware after an interruption",
                        d.id,
                        s.id
                    );
                }
                _ => panic!("{}: {} has no way out", d.id, s.id),
            }
            assert!(!s.done.is_empty(), "{}: {} has no done", d.id, s.id);
        }
    }
}
