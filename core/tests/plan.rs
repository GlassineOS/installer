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

/// A snapshot that lists the shared EFI system partition among Windows's own:
/// replacing Windows would remove it, which its rule doesn't allow.
#[test]
fn the_plan_checks_each_partition_against_its_rule() {
    let ds = [valid(&pc())];
    let refs: Vec<&Description> = ds.iter().collect();
    let mut f = facts("pc-windows");
    f.systems[0].partitions.insert(0, 1);
    let p = plan(&refs, &f, &recognize(&refs, &f), &input(&[("systems.1", "replace")], &[], true));
    assert_eq!(p.status, Status::Refused);
    assert!(
        p.reasons.iter().any(|r| r.contains("disk 0 partition 1") && r.contains("never-write")),
        "{:?}",
        p.reasons
    );
}

/// 001 FR-009: one image per disk the plan writes to, and only those. Two
/// fixed disks, each with a system; GlassineOS goes on one of them.
#[test]
fn only_the_disk_written_to_is_imaged() {
    let p = run("pc-two-disks-first", "pc-two-disks", input(&[], &[], true));
    let imaged: Vec<u32> = p.backup.images.iter().map(|i| i.index).collect();
    assert_eq!(imaged, vec![0]);
    assert_eq!(p.backup.images[0].estimate, "at-most", "inspecting doesn't see used blocks");
    assert_eq!(p.backup.images[0].estimate_bytes, Some(549755813888));
    assert_eq!(p.backup.target.kind, "installer-media");
    assert_eq!(p.status, Status::Ready);

    let p = run("pc-two-disks-second", "pc-two-disks", input(&[("target-disk", "disk1")], &[], true));
    let imaged: Vec<u32> = p.backup.images.iter().map(|i| i.index).collect();
    assert_eq!(imaged, vec![1]);
    // Disk 0's partitions are neither imaged nor written.
    assert!(p.partitions.iter().all(|x| !x.partition.starts_with("disk 0")), "{:#?}", p.partitions);
    assert_eq!(p.status, Status::Ready);
}

/// Replacing the system on the other disk writes that disk too: both are imaged.
#[test]
fn every_disk_written_to_is_imaged() {
    let ds = [valid(&pc())];
    let refs: Vec<&Description> = ds.iter().collect();
    let f = facts("pc-two-disks");
    let p = plan(&refs, &f, &recognize(&refs, &f), &input(&[("systems.2", "replace")], &[], true));
    let imaged: Vec<u32> = p.backup.images.iter().map(|i| i.index).collect();
    assert_eq!(imaged, vec![0, 1]);
}

/// Each image goes somewhere other than the disks being changed.
#[test]
fn the_backup_never_goes_to_a_disk_it_images() {
    let ds = [valid(&pc())];
    let refs: Vec<&Description> = ds.iter().collect();
    let f = facts("pc-two-disks");
    let p = plan(&refs, &f, &recognize(&refs, &f), &input(&[("backup-to", "disk0")], &[], true));
    assert_eq!(p.status, Status::Refused);
    assert!(p.reasons.iter().any(|r| r.contains("can't go to disk 0")), "{:?}", p.reasons);
    let p = plan(&refs, &f, &recognize(&refs, &f), &input(&[("backup-to", "disk1")], &[], true));
    assert_eq!(p.status, Status::Ready, "{:?}", p.reasons);
    assert_eq!(p.backup.target.disk, Some(1));
}

/// FR-009a: switching the backup off is a choice of its own, with its consequence.
#[test]
fn the_backup_can_be_switched_off() {
    let p = run(
        "pc-windows-replace-no-backup",
        "pc-windows",
        input(&[("systems.1", "replace"), ("backup", "off")], &[], true),
    );
    assert_eq!(p.status, Status::Ready);
    assert!(p.backup.images.is_empty());
    assert!(p.backup.consequence.contains("no way back"));
    // A firmware variable is outside the disks: still kept.
    let b = p.boot_settings.iter().find(|b| b.id == "boot-entries").unwrap();
    assert!(b.actions.contains(&"back-up".to_string()));
    assert_eq!(p.backup.escapes_lost, vec!["remove-system.1".to_string()]);
    assert!(p.choices.iter().any(|c| c.choice == "backup" && c.value == "off" && !c.default));
    let p = run(
        "noteair-no-backup",
        "noteair-stock-fastboot",
        input(&[("backup", "off")], &[("emergency-cable", true)], true),
    );
    assert!(p.backup.images.is_empty());
    assert_eq!(p.space.backup_bytes, 0);
    // The boot settings kept in the disk image aren't backed up without it.
    assert!(
        p.boot_settings.iter().all(|b| !b.actions.contains(&"back-up".to_string())),
        "{:#?}",
        p.boot_settings
    );
    let txt = render(&p, Some(&valid(&noteair())));
    assert!(txt.contains("The backup is off: no disk is imaged"), "{txt}");
    assert!(!txt.contains("Back up the device's whole storage"), "{txt}");
}

/// The opt-out's consequence is in the record before anyone switches it off.
#[test]
fn the_consequence_is_stated_with_the_backup_on() {
    let p = run("pc-windows-keep", "pc-windows", Input::default());
    assert_eq!(p.backup.choice, "full");
    assert!(p.backup.consequence.contains("no way back"));
}

/// An image known only as at most the disk's size proves nothing about room.
#[test]
fn an_upper_bound_never_refuses() {
    let ds = [valid(&pc())];
    let refs: Vec<&Description> = ds.iter().collect();
    let f = facts("pc-two-disks");
    let mut i = input(&[], &[], true);
    i.free_bytes = Some(100 << 30);
    let p = plan(&refs, &f, &recognize(&refs, &f), &i);
    assert_eq!(p.status, Status::Ready, "{:?}", p.reasons);
    assert_eq!(p.space.backup_at_most, 549755813888);
    assert!(p.space.total_at_least < 100 << 30);
}

/// The Note Air's image is its whole storage, sized by its description.
#[test]
fn note_air_images_its_whole_storage() {
    let p = run("noteair-keep-stock", "noteair-stock-fastboot", Input::default());
    assert_eq!(p.backup.images.len(), 1);
    let i = &p.backup.images[0];
    assert_eq!((i.estimate.as_str(), i.estimate_bytes), ("about", Some(9_200_000_000)));
    assert_eq!(p.backup.target.kind, "computer");
    assert_eq!(p.untouched_partitions, 0);
}

/// FR-003: a format-1 description says nothing about whole disks, so the
/// full backup can't be planned for it; with the backup off it can.
#[test]
fn a_format_1_description_has_no_full_backup() {
    let d = valid(&text("core/tests/fixtures/descriptions/uefi-pc-format1.toml"));
    let refs = [&d];
    let f = facts("pc-windows");
    let p = plan(&refs, &f, &recognize(&refs, &f), &input(&[], &[], true));
    assert_eq!(p.status, Status::Refused);
    assert!(p.reasons.iter().any(|r| r.contains("whole disk")), "{:?}", p.reasons);
    let p = plan(&refs, &f, &recognize(&refs, &f), &input(&[("backup", "off")], &[], true));
    assert_eq!(p.status, Status::Ready, "{:?}", p.reasons);
}
