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

/// The Note Air as it will be once its GlassineOS recovery is built.
fn noteair_with_recovery() -> String {
    mutate(
        &noteair(),
        "status = \"needs\"\nwith = \"the GlassineOS recovery, which isn't built yet\"",
        "status = \"have\"",
    )
}

/// Plans `input` for the stock Note Air in fastboot from `description`.
fn plan_with(description: &str, i: &Input) -> glassine_installer::plan::Plan {
    let ds = [valid(description), valid(&pc())];
    let refs: Vec<&Description> = ds.iter().collect();
    let f = facts("noteair-stock-fastboot");
    plan(&refs, &f, &recognize(&refs, &f), i)
}

/// 001 FR-016a: without its GlassineOS recovery the Note Air is refused for
/// people, with what is missing; the steps stay listed.
#[test]
fn note_air_keep_stock() {
    let p = run("noteair-keep-stock", "noteair-stock-fastboot", Input::default());
    assert_eq!(p.status, Status::Refused);
    assert!(
        p.reasons
            .iter()
            .any(|r| r.contains("the GlassineOS recovery, which isn't built yet") && r.contains("write-recovery")),
        "{:?}",
        p.reasons
    );
    assert!(p.developer_may);
    assert!(p.left_out.is_empty());
    assert!(p.steps.iter().any(|s| s.id == "write-recovery"));
    assert_eq!(p.questions[0].hardware, "emergency-cable");
    assert!(p.steps.iter().all(|s| s.id != "retire-stock"));
    let userdata = p.partitions.iter().find(|x| x.partition == "userdata").unwrap();
    assert!(userdata.loses_files);
    assert!(!p.space.exact, "artifact sizes are unknown, so the space is 'at least'");
    // Once the recovery is there, the plan asks its question first.
    let p = plan_with(&noteair_with_recovery(), &Input::default());
    assert_eq!(p.status, Status::NeedsAnswers);
    assert_eq!(p.questions[0].hardware, "emergency-cable");
}

/// 001 FR-016a, 002 FR-022: a developer's plan leaves out the step that
/// writes the recovery, names it and what the recovery partition holds
/// instead, and goes on; set-slot no longer waits for it.
#[test]
fn note_air_without_stock_for_a_developer() {
    let p = run(
        "noteair-no-stock-developer",
        "noteair-stock-fastboot",
        input(&[("keep-stock", "no")], &[("emergency-cable", true)], true),
    );
    assert_eq!(p.status, Status::Ready);
    assert!(p.steps.iter().any(|s| s.id == "retire-stock"));
    assert!(p.steps.iter().all(|s| s.id != "write-recovery"));
    assert!(p.steps.iter().any(|s| s.id == "set-slot"));
    assert_eq!(p.left_out.len(), 1);
    let l = &p.left_out[0];
    assert_eq!(
        (l.step.as_str(), l.artifact.as_str(), l.capability.as_str()),
        ("write-recovery", "recovery", "recovery")
    );
    assert_eq!(l.missing.as_deref(), Some("the GlassineOS recovery, which isn't built yet"));
    assert_eq!(l.holds_instead[0].partition, "recovery");
    let h = &l.holds_instead[0];
    assert_eq!(
        (h.step.as_deref(), h.artifact.as_deref()),
        (Some("write-ram-environment"), Some("ram-environment"))
    );
    // The way out that named the left-out step goes on where its interruption would.
    let v = p.steps.iter().find(|s| s.id == "write-vendor").unwrap();
    assert_eq!(v.on_interruption.way_out, "step set-slot");
    assert!(
        p.steps
            .iter()
            .all(|s| !s.on_failure.way_out.contains("write-recovery") && !s.on_interruption.way_out.contains("write-recovery"))
    );
    let txt = render(&p, Some(&valid(&noteair())));
    assert!(txt.contains("write-recovery: writes the recovery, which the release doesn't have (the GlassineOS recovery, which isn't built yet). The device ends without the GlassineOS recovery."), "{txt}");
    assert!(
        txt.contains("recovery holds the ram-environment, which step write-ram-environment writes instead."),
        "{txt}"
    );
}

/// Without a missing artifact nothing is left out.
#[test]
fn nothing_is_left_out_once_the_recovery_is_there() {
    let p = plan_with(&noteair_with_recovery(), &input(&[], &[("emergency-cable", true)], true));
    assert_eq!(p.status, Status::Ready, "{:?}", p.reasons);
    assert!(p.left_out.is_empty());
    assert!(p.steps.iter().any(|s| s.id == "write-recovery"));
}

#[test]
fn untried_write_steps_are_for_developers_only() {
    let p = run("noteair-cable-yes", "noteair-stock-fastboot", input(&[], &[("emergency-cable", true)], false));
    assert_eq!(p.status, Status::Refused, "people: refused until the recovery is there");
    assert!(p.untried_steps.contains(&"replace-boot-loader".to_string()));
    let p = plan_with(&noteair_with_recovery(), &input(&[], &[("emergency-cable", true)], false));
    assert_eq!(p.status, Status::DeveloperOnly);
    assert!(p.untried_steps.contains(&"replace-boot-loader".to_string()));
    // The developer's plan with the question answered.
    let p = run(
        "noteair-developer-cable-yes",
        "noteair-stock-fastboot",
        input(&[], &[("emergency-cable", true)], true),
    );
    assert_eq!(p.status, Status::Ready, "{:?}", p.reasons);
    assert_eq!(p.left_out.len(), 1);
    // and before it is answered
    let p = run("noteair-developer", "noteair-stock-fastboot", input(&[], &[], true));
    assert_eq!(p.status, Status::NeedsAnswers);
}

/// 002 FR-014b: a step names a renamed partition by either name, resolved on
/// whichever table inspecting finds: the stock one before the table step, the
/// A/B one when an installation goes on after it.
#[test]
fn renamed_partitions_resolve_on_either_table() {
    // On the stock table write-vendor reads `vendor`, and a step naming
    // `vendor_a` finds it too.
    let p = plan_with(&noteair(), &input(&[], &[("emergency-cable", true)], true));
    assert!(p.reasons.iter().all(|r| !r.contains("didn't find")), "{:?}", p.reasons);
    let v = p.partitions.iter().find(|x| x.partition == "vendor").unwrap();
    assert_eq!(v.actions, vec!["reads".to_string()]);
    let t = mutate(&noteair(), "reads = [\"vendor\"]", "reads = [\"vendor_a\"]");
    let p = plan_with(&t, &input(&[], &[("emergency-cable", true)], true));
    assert!(p.reasons.iter().all(|r| !r.contains("didn't find")), "{:?}", p.reasons);
    assert!(
        p.partitions
            .iter()
            .any(|x| x.partition == "vendor" && x.steps.contains(&"write-vendor".to_string()))
    );
    // On the A/B table (`vendor_a`), the step's `vendor` resolves there.
    let ds = [valid(&noteair())];
    let refs: Vec<&Description> = ds.iter().collect();
    let mut f = facts("noteair-stock-fastboot");
    let fb = f.fastboot.as_mut().unwrap();
    let size = fb.variables.remove("partition-size:vendor").expect("the stock facts list vendor");
    fb.variables.insert("partition-size:vendor_a".into(), size);
    let p = plan(&refs, &f, &recognize(&refs, &f), &input(&[], &[("emergency-cable", true)], true));
    assert!(p.reasons.iter().all(|r| !r.contains("didn't find")), "{:?}", p.reasons);
    let v = p.partitions.iter().find(|x| x.partition == "vendor_a").unwrap();
    assert_eq!((v.class.as_str(), v.steps.clone()), ("never-write", vec!["write-vendor".to_string()]));
    // An unknown name is still not found.
    let t = mutate(&noteair(), "reads = [\"vendor\"]", "reads = [\"vendor_c\"]");
    let p = plan_with(&t, &input(&[], &[("emergency-cable", true)], true));
    assert!(p.reasons.iter().any(|r| r.contains("vendor_c (not found)")), "{:?}", p.reasons);
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

const GIB: u64 = 1 << 30;

/// 002 FR-014a, FR-019a: on a PC the boot (2 GiB) and system (16 GiB)
/// partitions are fixed and userdata takes the rest, at least 8 GiB: a disk
/// that can't give 26 GiB is refused with what it lacks, release or not.
#[test]
fn a_disk_too_small_for_glassine_is_refused() {
    let mut i = input(&[], &[], true);
    i.release = Some(release("base-8gib"));
    let p = run("pc-small-disk", "pc-small-disk", i);
    assert_eq!(p.status, Status::Refused);
    let d = p.space.disk.as_ref().unwrap();
    assert_eq!((d.needs, d.free, d.lacks, d.exact), (26 * GIB, 6 * GIB, 20 * GIB, true));
    assert!(
        p.reasons.iter().any(|r| r.contains("disk 0 is too small") && r.contains("it lacks 20.0 GiB")),
        "{:?}",
        p.reasons
    );
    // The release's base fits the fixed system partition.
    let m = p.space.minimums.iter().find(|m| m.role == "base").unwrap();
    assert_eq!(
        (m.partition.as_str(), m.at_least, m.size, m.lacks),
        ("glassine-system", Some(8 * GIB), Some(16 * GIB), 0)
    );
    // Without a release it is refused the same: the sizes are the description's.
    let p = plan_pc("pc-small-disk", &input(&[], &[], true));
    assert_eq!(p.space.disk.as_ref().unwrap().lacks, 20 * GIB);
}

/// Without a release (a dry run, a test) the least sizes aren't known yet:
/// they never refuse, and the plan says they aren't known.
#[test]
fn without_a_release_the_least_is_not_known_yet() {
    let p = plan_pc("pc-windows", &input(&[], &[], true));
    assert_eq!(p.status, Status::Ready, "{:?}", p.reasons);
    assert!(p.space.minimums.iter().all(|m| m.at_least.is_none()));
    let roles: Vec<&str> = p.space.minimums.iter().map(|m| m.role.as_str()).collect();
    assert_eq!(roles, ["kernel", "base"]);
    let txt = render(&p, Some(&valid(&pc())));
    assert!(
        txt.contains("glassine-system (for the base): 16.0 GiB; what the release asks is not known yet"),
        "{txt}"
    );
    assert!(
        txt.contains("glassine-boot (for the kernel): 2.0 GiB; what the release asks is not known yet"),
        "{txt}"
    );
    // A release that doesn't state them is the same.
    let mut i = input(&[], &[], true);
    i.release = Some(glassine_installer::release::Release::parse(r#"{"release_format": 1, "artifacts": {"kernel": {}}}"#).unwrap());
    assert_eq!(plan_pc("pc-windows", &i).status, Status::Ready);
}

/// 002 FR-014a: a relative size whose least is the release's counts that
/// least on the disk; while no release states it, it counts as nothing and
/// the disk's need is only "at least". (The shipped PC description sizes its
/// system partition fixed; this one makes it take the rest.)
#[test]
fn a_relative_size_counts_the_release_least() {
    let t = mutate(
        &pc(),
        "{ name = \"glassine-system\", size = \"16GiB\", at_least = \"release:base\", role = \"system\" }",
        "{ name = \"glassine-system\", size = \"rest-of-free-space\", at_least = \"release:base\", role = \"system\" }",
    );
    let t = mutate(
        &t,
        "{ name = \"glassine-userdata\", size = \"rest-of-free-space\", at_least = \"8GiB\" }",
        "{ name = \"glassine-userdata\", size = \"8GiB\" }",
    );
    let ds = [valid(&t)];
    let refs: Vec<&Description> = ds.iter().collect();
    let f = facts("pc-small-disk");
    let p = plan(&refs, &f, &recognize(&refs, &f), &input(&[], &[], true));
    let d = p.space.disk.as_ref().unwrap();
    // 2 GiB boot and 8 GiB userdata fixed; the base's least not known yet.
    assert_eq!((d.needs, d.exact), (10 * GIB, false));
    let m = p.space.minimums.iter().find(|m| m.role == "base").unwrap();
    assert_eq!((m.at_least, m.size), (None, None));
    let mut i = input(&[], &[], true);
    i.release = Some(release("base-8gib"));
    let p = plan(&refs, &f, &recognize(&refs, &f), &i);
    let d = p.space.disk.as_ref().unwrap();
    assert_eq!((d.needs, d.free, d.lacks, d.exact), (18 * GIB, 6 * GIB, 12 * GIB, true));
}

/// 002 FR-019a, boot 010 FR-029: a release whose kernel needs more than the
/// boot partition, or whose base needs more than the system partition, is
/// refused with what each lacks.
#[test]
fn a_release_that_outgrows_the_pc_partitions_is_refused() {
    let mut i = input(&[], &[], true);
    i.release = Some(release("pc-too-big"));
    let p = run("pc-release-too-big", "pc-windows", i);
    assert_eq!(p.status, Status::Refused);
    let lacks: Vec<(&str, u64)> = p.space.minimums.iter().map(|m| (m.partition.as_str(), m.lacks)).collect();
    assert_eq!(lacks, [("glassine-boot", GIB), ("glassine-system", 4 * GIB)]);
    assert!(
        p.reasons.iter().any(|r| r.contains("glassine-boot is too small for this release")),
        "{:?}",
        p.reasons
    );
    assert!(
        p.reasons.iter().any(|r| r.contains("glassine-system is too small for this release")),
        "{:?}",
        p.reasons
    );
}

/// 001 FR-012, FR-012a: the PC plan creates the boot, system and userdata
/// partitions; userdata is the description's data partition (002 FR-018c).
#[test]
fn the_pc_plan_creates_userdata() {
    let p = plan_pc("pc-windows", &input(&[], &[], true));
    let created: Vec<&str> = p
        .partitions
        .iter()
        .filter(|x| x.actions.iter().any(|a| a == "creates"))
        .map(|x| x.partition.as_str())
        .collect();
    assert!(created.contains(&"glassine-userdata"), "{created:?}");
    assert_eq!(valid(&pc()).data_partition.unwrap().name, "glassine-userdata");
}

/// Boot 010 FR-036, SC-016: with Secure Boot on the install is refused, for a
/// developer too, with the reason and how to switch it off; set-up mode and
/// an unknown state are said and don't refuse.
#[test]
fn secure_boot_on_refuses_a_pc_install() {
    let p = run("pc-secure-boot-on", "pc-secure-boot-on", input(&[], &[], true));
    assert_eq!(p.status, Status::Refused);
    assert!(!p.developer_may);
    assert_eq!(p.reasons, [glassine_installer::plan::SECURE_BOOT_ON]);
    let r = &p.reasons[0];
    assert!(r.contains("Secure Boot is on") && r.contains("set Secure Boot to Disabled"), "{r}");
    // Off: no word of it.
    let p = plan_pc("pc-windows", &input(&[], &[], true));
    assert!(p.notes.is_empty() && p.reasons.is_empty(), "{:?} {:?}", p.notes, p.reasons);
    // Unknown (the variables can't be read): said, never refused for it.
    let p = run("pc-secure-boot-unknown", "pc-secure-boot-unknown", input(&[], &[], true));
    assert_eq!(p.status, Status::Ready, "{:?}", p.reasons);
    assert_eq!(p.notes, [glassine_installer::plan::SECURE_BOOT_UNKNOWN]);
    // Set-up mode starts any loader: said, not refused.
    let mut f = facts("pc-windows");
    f.firmware.as_mut().unwrap().secure_boot = Some("setup".into());
    let ds = [valid(&pc())];
    let refs: Vec<&Description> = ds.iter().collect();
    let p = plan(&refs, &f, &recognize(&refs, &f), &input(&[], &[], true));
    assert_eq!(p.status, Status::Ready, "{:?}", p.reasons);
    assert_eq!(p.notes, [glassine_installer::plan::SECURE_BOOT_SETUP]);
}

/// Plans `input` for the PC facts `facts_name`, with no golden file.
fn plan_pc(facts_name: &str, i: &Input) -> glassine_installer::plan::Plan {
    let ds = [valid(&noteair()), valid(&pc())];
    let refs: Vec<&Description> = ds.iter().collect();
    let f = facts(facts_name);
    plan(&refs, &f, &recognize(&refs, &f), i)
}

/// A fixed partition is checked the same way: a release whose base outgrows
/// the Note Air's slot-B system partition (3 GiB) refuses the plan.
#[test]
fn a_release_that_outgrows_a_fixed_partition_is_refused() {
    let mut i = input(&[], &[("emergency-cable", true)], true);
    i.release = Some(release("base-8gib"));
    let p = run("noteair-release-too-big", "noteair-stock-fastboot", i);
    assert_eq!(p.status, Status::Refused);
    let m = &p.space.minimums[0];
    assert_eq!(
        (m.partition.as_str(), m.at_least, m.size, m.lacks),
        ("system_b", Some(8 * GIB), Some(3 * GIB), 5 * GIB)
    );
    assert!(
        p.reasons
            .iter()
            .any(|r| r.contains("system_b is too small for this release") && r.contains("it lacks 5.0 GiB")),
        "{:?}",
        p.reasons
    );
    // Without the release it isn't known, and isn't a reason.
    let p = run("noteair-cable-yes", "noteair-stock-fastboot", input(&[], &[("emergency-cable", true)], false));
    assert_eq!(p.space.minimums[0].at_least, None);
    assert!(p.reasons.iter().all(|r| !r.contains("too small")), "{:?}", p.reasons);
}

/// The manifest is read by its format; keys an installer doesn't know are ignored.
#[test]
fn the_release_manifest_is_read_by_its_format() {
    use glassine_installer::release::Release;
    assert_eq!(release("base-8gib").min_partition_bytes("base"), Some(8 * GIB));
    assert!(Release::parse(r#"{"release_format": 2}"#).unwrap_err().contains("reads up to 1"));
    assert!(Release::parse(r#"{"artifacts": {}}"#).is_err());
    let r = Release::parse(r#"{"release_format": 1, "later": true, "artifacts": {"base": {"min_partition_bytes": 5, "other": 1}}}"#).unwrap();
    assert_eq!(r.min_partition_bytes("base"), Some(5));
}

/// 001 FR-016c: a Windows volume hibernated or left by Fast Startup refuses a
/// plan that writes to its disk, for a developer too, with how to shut
/// Windows down fully and why; replacing it, or writing to another disk
/// only, is said and doesn't refuse.
#[test]
fn a_hibernated_windows_refuses_writing_next_to_it() {
    let p = run("pc-windows-hibernated-keep", "pc-windows-hibernated", input(&[], &[], true));
    assert_eq!(p.status, Status::Refused);
    assert!(!p.developer_may);
    assert_eq!(p.reasons.len(), 1, "{:?}", p.reasons);
    let r = &p.reasons[0];
    assert!(
        r.contains("disk 0 partition 3") && r.contains("hibernated") && r.contains("hold Shift") && r.contains("can lose data"),
        "{r}"
    );
    assert!(r.ends_with(glassine_installer::plan::WINDOWS_SHUT_DOWN), "{r}");
    // Replaced: said, not refused; the backup keeps it whole.
    let p = run(
        "pc-windows-hibernated-replace",
        "pc-windows-hibernated",
        input(&[("systems.1", "replace")], &[], true),
    );
    assert_eq!(p.status, Status::Ready, "{:?}", p.reasons);
    assert!(
        p.notes
            .iter()
            .any(|n| n.contains("The plan replaces it") && n.contains("the backup holds the volume whole")),
        "{:?}",
        p.notes
    );
    // Dirty or a log not replayed: the same.
    for (state, says) in [("dirty", "marked dirty"), ("log-not-clean", "its log holds changes")] {
        let mut f = facts("pc-windows-hibernated");
        f.storage.as_mut().unwrap().disks[0].partitions[2].fs.as_mut().unwrap().unclean = Some(state.into());
        let ds = [valid(&pc())];
        let refs: Vec<&Description> = ds.iter().collect();
        let p = plan(&refs, &f, &recognize(&refs, &f), &input(&[], &[], true));
        assert_eq!(p.status, Status::Refused);
        assert!(p.reasons[0].contains(says), "{:?}", p.reasons);
    }
    // On a disk the plan doesn't write (GlassineOS goes on the second disk): said only.
    let mut f = facts("pc-two-disks");
    f.storage.as_mut().unwrap().disks[0].partitions[2].fs.as_mut().unwrap().unclean = Some("hibernated".into());
    let ds = [valid(&pc())];
    let refs: Vec<&Description> = ds.iter().collect();
    let p = plan(&refs, &f, &recognize(&refs, &f), &input(&[("target-disk", "disk1")], &[], true));
    assert!(p.reasons.iter().all(|r| !r.contains("hibernated")), "{:?}", p.reasons);
    assert!(p.notes.iter().any(|n| n.contains("The plan doesn't write to disk 0")), "{:?}", p.notes);
}
