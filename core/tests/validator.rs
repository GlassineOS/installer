// SPDX-License-Identifier: GPL-2.0-or-later
//! SC-002: for every rule of FR-002 and FR-025 (and the format's other
//! checkable rules) a description that breaks it, rejected with the key's path.

mod common;
use common::*;
use glassine_installer::validate::{Loaded, check_set, load};

/// The rule's broken description is rejected with a problem at `path`.
fn rejects(t: &str, path: &str) {
    match load(t) {
        Loaded::Invalid(ps) => assert!(ps.iter().any(|p| p.path == path), "no problem at {path:?}; got {ps:#?}"),
        Loaded::Valid(_) => panic!("accepted, but should fail at {path:?}"),
        Loaded::TooNew { .. } => panic!("read as too new"),
    }
}

fn na(old: &str, new: &str, path: &str) {
    rejects(&mutate(&noteair(), old, new), path);
}

fn pcm(old: &str, new: &str, path: &str) {
    rejects(&mutate(&pc(), old, new), path);
}

#[test]
fn the_shipped_descriptions_are_valid() {
    let a = valid(&noteair());
    let b = valid(&pc());
    assert!(check_set(&[&a, &b]).is_empty());
}

// ---- FR-002: structure --------------------------------------------------------

#[test]
fn unknown_key() {
    na(
        "backend = \"android-bootloader\"",
        "backend = \"android-bootloader\"\ncolour = \"red\"",
        "colour",
    );
}

#[test]
fn unknown_nested_key() {
    na("id = \"write-base\"\n", "id = \"write-base\"\ncolour = \"red\"\n", "steps[4].colour");
}

#[test]
fn missing_required_key() {
    na("name = \"Onyx Boox Note Air\"\nbackend", "backend", "name");
}

#[test]
fn missing_nested_key() {
    na("id = \"write-base\"\njob = \"install\"\n", "id = \"write-base\"\n", "steps[4].job");
}

#[test]
fn wrong_type() {
    pcm("generic = true", "generic = \"yes\"", "generic");
}

#[test]
fn unknown_enum_value() {
    na("kind = \"write-base\"", "kind = \"dance\"", "steps[4].kind");
}

#[test]
fn missing_kind_parameter() {
    na(
        "params = { artifact = \"kernel\", partition = \"boot_b\" }",
        "params = { artifact = \"kernel\" }",
        "steps[3].params.partition",
    );
}

#[test]
fn unknown_kind_parameter() {
    na(
        "params = { slot = \"b\", pin = true }",
        "params = { slot = \"b\", pin = true, speed = 3 }",
        "steps[6].params.speed",
    );
}

#[test]
fn one_test_per_condition() {
    na(
        "equals = \"SDM EMMC\"\nrequired = true",
        "equals = \"SDM EMMC\"\none-of = [\"x\"]\nrequired = true",
        "identity[0]",
    );
}

#[test]
fn verified_or_untried() {
    na(
        "required = false\nuntried = \"the stock boot loader answered getvar all with it in development (2026-03-17); not a recorded run\"",
        "required = false",
        "identity[2]",
    );
}

#[test]
fn every_problem_at_once() {
    let t = mutate(
        &noteair(),
        "backend = \"android-bootloader\"",
        "backend = \"android-bootloader\"\ncolour = \"red\"",
    );
    let t = mutate(&t, "kind = \"write-base\"", "kind = \"dance\"");
    let Loaded::Invalid(ps) = load(&t) else { panic!() };
    assert!(ps.iter().any(|p| p.path == "colour") && ps.iter().any(|p| p.path == "steps[4].kind"), "{ps:#?}");
}

#[test]
fn format_first() {
    let t = mutate(&noteair(), "format = 2\nid = \"onyx-noteair\"", "id = \"onyx-noteair\"\nformat = 2");
    rejects(&t, "format");
}

#[test]
fn unknown_format() {
    na("format = 2", "format = 0", "format");
}

#[test]
fn not_toml() {
    rejects("format = 1\nid = ", "(top)");
}

/// Scenario 2: a newer format is refused as too new, never as invalid, and
/// nothing of it is read.
#[test]
fn newer_format_is_too_new_not_invalid() {
    let t = mutate(&noteair(), "format = 2", "format = 3\nnew_key_of_format_3 = true");
    assert!(matches!(load(&t), Loaded::TooNew { format: 3 }));
}

/// FR-003: a reader reads every earlier format too.
#[test]
fn format_1_still_reads() {
    for f in ["onyx-noteair-format1", "uefi-pc-format1"] {
        let d = valid(&text(&format!("core/tests/fixtures/descriptions/{f}.toml")));
        assert_eq!(d.format, 1);
    }
}

/// FR-003: what format 2 added is not in format 1.
#[test]
fn format_1_has_no_disk_role() {
    let t = text("core/tests/fixtures/descriptions/onyx-noteair-format1.toml");
    let t = mutate(
        &t,
        "[[partitions]]\nselect = { name = \"userdata\" }",
        "[[partitions]]\nselect = { role = \"disk\" }\nclass = \"back-up\"\nback_up = { mode = \"ram\", means = \"glassine-read\", check = \"read-back-compare\" }\n\n[[partitions]]\nselect = { name = \"userdata\" }",
    );
    rejects(&t, "partitions[6].select.role");
}

// ---- FR-025: names the vocabularies or the backend don't have ----------------------

#[test]
fn unknown_fact_key() {
    na("key = \"fastboot.var.variant\"", "key = \"fastboot.colour\"", "identity[0].key");
}

#[test]
fn mode_kind_the_backend_lacks() {
    na(
        "id = \"glassine\"\nkind = \"glassine-usb\"",
        "id = \"glassine\"\nkind = \"local\"",
        "modes[5].kind",
    );
}

#[test]
fn step_kind_the_backend_lacks_with_parameters() {
    // A kind of another backend, with parameters that don't fit it: both are named.
    let t = mutate(&noteair(), "kind = \"write-base\"", "kind = \"add-boot-entry\"");
    rejects(&t, "steps[4].kind");
    rejects(&t, "steps[4].params.artifact");
}

#[test]
fn step_kind_the_backend_lacks() {
    na("kind = \"back-up\"", "kind = \"remove-partitions\"", "steps[0].kind");
}

#[test]
fn boot_setting_kind_the_backend_lacks() {
    na("kind = \"slot-flags\"", "kind = \"firmware-boot-entry\"", "boot_settings[1].kind");
}

#[test]
fn departure_for_another_mode_kind() {
    na(
        "can = [\"identify\"]\nrestarts_into = [\"fastboot-stock\", \"emergency\"]",
        "can = [\"identify\"]\nrestarts_into = [\"fastboot-stock\", \"emergency\"]\ndepartures = [{ kind = \"fresh-boot-only\", untried = \"x\" }]",
        "modes[3].departures[0].kind",
    );
}

#[test]
fn unknown_operation() {
    na(
        "operation = \"boot-image\"",
        "operation = \"explode\"",
        "modes[1].departures[3].params.operation",
    );
}

#[test]
fn unknown_role() {
    na("writes = [\"role:partition-table\"]", "writes = [\"role:swap\"]", "steps[2].writes[0]");
}

#[test]
fn backup_means_of_another_mode() {
    na(
        "back_up = { mode = \"ram\", means = \"glassine-read\", check = \"read-back-compare\", size = 8600000000 }",
        "back_up = { mode = \"ram\", means = \"block-read\", check = \"read-back-compare\", size = 8600000000 }",
        "partitions[7].back_up.means",
    );
}

// ---- FR-007a, FR-008: identity -------------------------------------------------------

#[test]
fn refines_itself() {
    pcm("generic = true", "refines = \"uefi-pc\"", "refines");
}

#[test]
fn refines_an_unknown_description() {
    let d = valid(&refined_pc("example-laptop", "Example Computers", "Example 13"));
    let problems = check_set(&[&d]);
    assert!(problems.iter().any(|(id, p)| id == "example-laptop" && p.path == "refines"), "{problems:?}");
}

#[test]
fn one_required_signal_is_not_enough() {
    na("equals = 33554432\nrequired = true", "equals = 33554432\nrequired = false", "identity");
}

#[test]
fn two_signals_of_one_kind_are_not_enough() {
    na(
        "key = \"storage.partition.onyxconfig.size\"\nequals = 33554432",
        "key = \"fastboot.var.secure\"\nequals = \"no\"",
        "identity",
    );
}

#[test]
fn identity_never_reads_a_state_signal() {
    na(
        "key = \"fastboot.var.variant\"\nequals = \"SDM EMMC\"",
        "key = \"fastboot.var.product\"\nequals = \"sdm660\"",
        "identity[0].key",
    );
}

#[test]
fn identity_never_reads_a_serial() {
    na("key = \"fastboot.var.hw-revision\"", "key = \"fastboot.var.serialno\"", "identity[2].key");
}

#[test]
fn identity_never_reads_a_unit_unique_fact() {
    pcm("key = \"cpu.arch\"", "key = \"board.serial\"", "identity[1].key");
}

#[test]
fn test_fits_the_fact() {
    na("equals = 33554432", "equals = \"32 MiB\"", "identity[1].equals");
}

// ---- FR-009: states --------------------------------------------------------------------

#[test]
fn first_state_is_stock() {
    na("[[states]]\nid = \"stock\"", "[[states]]\nid = \"factory\"", "states[0].id");
}

#[test]
fn condition_mode_exists() {
    na(
        "equals = \"sdm660\"\nmode = \"fastboot-stock\"",
        "equals = \"sdm660\"\nmode = \"bootloader\"",
        "states[0].conditions[0].mode",
    );
}

#[test]
fn condition_observable_in_its_mode() {
    na(
        "equals = \"sdm660\"\nmode = \"fastboot-stock\"",
        "equals = \"sdm660\"\nmode = \"android\"",
        "states[0].conditions[0].mode",
    );
}

// ---- FR-010, FR-013: modes and transitions ------------------------------------------------

#[test]
fn restart_target_exists() {
    na(
        "restarts_into = [\"glassine\", \"android\", \"emergency\"]",
        "restarts_into = [\"glassine\", \"android\", \"heaven\"]",
        "modes[1].restarts_into[2]",
    );
}

#[test]
fn installer_transition_needs_a_restart() {
    na(
        "restarts_into = [\"glassine\", \"android\", \"emergency\"]",
        "restarts_into = [\"glassine\", \"emergency\"]",
        "transitions[8].by",
    );
}

#[test]
fn by_hand_transition_needs_its_text() {
    na(
        "cable-to-emergency = \"Let the battery",
        "cable-to-emergency-old = \"Let the battery",
        "texts.transitions.cable-to-emergency",
    );
}

// ---- FR-014, FR-014a, FR-015, FR-016a: partitions -----------------------------------------

#[test]
fn resize_says_whether_it_destroys() {
    na("class = \"resize\"\ndestroys = true", "class = \"resize\"", "partitions[6].destroys");
}

/// Format 1: a replace rule names its own backup.
#[test]
fn replace_names_its_backup_in_format_1() {
    rejects(
        &mutate(
            &text("core/tests/fixtures/descriptions/onyx-noteair-format1.toml"),
            "class = \"replace\"\nback_up = { mode = \"ram\", means = \"glassine-read\", check = \"read-back-compare\", size = 1048576 }",
            "class = \"replace\"",
        ),
        "partitions[1].back_up",
    );
}

/// 001 FR-009: the whole disk says how its image is read.
#[test]
fn the_whole_disk_names_how_it_is_read() {
    na(
        "back_up = { mode = \"ram\", means = \"glassine-read\", check = \"read-back-compare\", size = 8600000000 }",
        "back_up = { with = \"role:partition-table\" }",
        "partitions[7].back_up",
    );
}

/// 001 FR-009: a job that writes images the disks first, in a step of its own.
#[test]
fn a_writing_job_backs_up_whole_disks() {
    na("backs_up = [\"role:disk\"]", "backs_up = [\"boot\"]", "steps");
}

#[test]
fn every_write_comes_after_the_disk_backup() {
    na(
        "after = [\"back-up\"]\nparams = { artifact = \"boot-loader\"",
        "after = []\nparams = { artifact = \"boot-loader\"",
        "steps[1].after",
    );
}

#[test]
fn a_description_has_a_rule_for_the_whole_disk() {
    pcm(
        "[[partitions]]\nselect = { role = \"disk\" }\nclass = \"back-up\"\nback_up = { mode = \"local\", means = \"block-read\", check = \"read-back-compare\" }\n",
        "",
        "partitions",
    );
}

/// FR-009a: the backup is the flow's choice, never a description's.
#[test]
fn the_backup_choice_is_reserved() {
    na("[[choices]]\nid = \"keep-stock\"", "[[choices]]\nid = \"backup\"", "choices[0].id");
}

#[test]
fn backup_mode_exists() {
    na(
        "back_up = { mode = \"ram\", means = \"glassine-read\", check = \"read-back-compare\", size = 8600000000 }",
        "back_up = { mode = \"edl\", means = \"glassine-read\", check = \"read-back-compare\", size = 8600000000 }",
        "partitions[7].back_up.mode",
    );
}

#[test]
fn step_beyond_its_rule() {
    na("writes = [\"boot_b\"]", "writes = [\"persist\"]", "steps[3].writes[0]");
}

/// A full backup reads every partition: never-write forbids writing, not reading.
#[test]
fn never_write_partitions_may_be_backed_up() {
    let t = mutate(&noteair(), "backs_up = [\"role:disk\"]", "backs_up = [\"xbl\", \"modemst1\", \"role:disk\"]");
    valid(&t);
}

#[test]
fn unselected_partition_is_never_write() {
    na("writes = [\"boot_b\"]", "writes = [\"some-other\"]", "steps[3].writes[0]");
}

#[test]
fn param_partition_is_in_the_steps_writes() {
    na(
        "params = { artifact = \"kernel\", partition = \"boot_b\" }",
        "params = { artifact = \"kernel\", partition = \"system_b\" }",
        "steps[3].params.partition",
    );
}

#[test]
fn replaced_only_after_its_backup() {
    na("backs_up = [\"role:disk\"]", "backs_up = [\"boot\"]", "steps[1].writes[0]");
}

#[test]
fn boot_setting_changed_only_after_its_backup() {
    pcm(
        "backs_up = [\"role:disk\"]\nboot_settings = [\"boot-entries\", \"boot-order\"]",
        "backs_up = [\"role:disk\"]\nboot_settings = [\"boot-order\"]",
        "steps[5].boot_settings[0]",
    );
}

#[test]
fn rule_condition_names_a_real_choice() {
    pcm(
        "when = { choice = \"systems\", equals = \"replace\" }\n\n[[partitions]]",
        "when = { choice = \"systems\", equals = \"wipe\" }\n\n[[partitions]]",
        "partitions[1].when",
    );
}

// ---- FR-021: choices ------------------------------------------------------------------------

#[test]
fn replacing_a_system_is_never_the_default() {
    pcm("default = \"keep\"", "default = \"replace\"", "choices[1].default");
}

#[test]
fn choices_are_outcomes_not_partitions() {
    let t = mutate(&noteair(), "options = [{ id = \"yes\" }", "options = [{ id = \"userdata\" }");
    let t = mutate(&t, "default = \"yes\"", "default = \"userdata\"");
    rejects(&t, "choices[0].options[0].id");
}

#[test]
fn choice_needs_its_text() {
    na("keep-stock = \"Keep the stock", "keep-stock-x = \"Keep the stock", "texts.choices.keep-stock");
}

// ---- FR-022, FR-023, FR-023a: steps ------------------------------------------------------------

#[test]
fn after_names_a_step() {
    na(
        "after = [\"back-up\"]\nparams = { artifact = \"boot-loader\"",
        "after = [\"backup\"]\nparams = { artifact = \"boot-loader\"",
        "steps[1].after[0]",
    );
}

#[test]
fn order_has_no_cycle() {
    na(
        "after = [\"back-up\"]\nparams = { artifact = \"boot-loader\"",
        "after = [\"back-up\", \"set-slot\"]\nparams = { artifact = \"boot-loader\"",
        "steps[1].after",
    );
}

#[test]
fn step_job_exists() {
    na(
        "id = \"write-base\"\njob = \"install\"",
        "id = \"write-base\"\njob = \"repair\"",
        "steps[4].job",
    );
}

#[test]
fn step_mode_can_write() {
    na(
        "id = \"write-base\"\njob = \"install\"\nkind = \"write-base\"\nmode = \"fastboot\"",
        "id = \"write-base\"\njob = \"install\"\nkind = \"write-base\"\nmode = \"glassine\"",
        "steps[4].mode",
    );
}

#[test]
fn done_names_its_mode() {
    na(
        "done = [{ key = \"installer.step.write-base\", present = true, mode = \"installer\" }]",
        "done = [{ key = \"installer.step.write-base\", present = true }]",
        "steps[4].done[0].mode",
    );
}

#[test]
fn special_hardware_is_named() {
    na(
        "escape.failure = { leaves = \"special-hardware\", hardware = \"emergency-cable\", way_out = { transition = \"cable-to-emergency\" } }\nescape.interruption = { leaves = \"previous-system-starts\"",
        "escape.failure = { leaves = \"special-hardware\", way_out = { transition = \"cable-to-emergency\" } }\nescape.interruption = { leaves = \"previous-system-starts\"",
        "steps[1].escape.failure.hardware",
    );
}

#[test]
fn a_way_out_needing_the_cable_says_so() {
    na(
        "escape.failure = { leaves = \"installer-can-finish\", way_out = { step = \"write-kernel\" } }",
        "escape.failure = { leaves = \"installer-can-finish\", way_out = { transition = \"cable-to-emergency\" } }",
        "steps[3].escape.failure.leaves",
    );
}

#[test]
fn way_out_step_exists() {
    na(
        "way_out = { step = \"write-base\" } }\npoint_of_no_return",
        "way_out = { step = \"write-floor\" } }\npoint_of_no_return",
        "steps[3].escape.interruption.way_out.step",
    );
}

#[test]
fn verified_names_a_state() {
    na(
        "verified = { record = \"GlassineOS/abl#1\", state = \"installed\"",
        "verified = { record = \"GlassineOS/abl#1\", state = \"daily\"",
        "modes[1].departures[1].verified.state",
    );
}

#[test]
fn verified_points_to_a_record() {
    na(
        "record = \"GlassineOS/abl#1\"",
        "record = \"my notes\"",
        "modes[1].departures[1].verified.record",
    );
}

// ---- FR-026 and what a description never holds --------------------------------------------------

#[test]
fn texts_have_no_markup() {
    na(
        "write-kernel = \"Write the GlassineOS kernel to slot B.\"",
        "write-kernel = \"Write the **GlassineOS** kernel.\"",
        "texts.steps.write-kernel",
    );
    na(
        "write-kernel = \"Write the GlassineOS kernel to slot B.\"",
        "write-kernel = \"Write the <b>kernel</b>.\"",
        "texts.steps.write-kernel",
    );
}

#[test]
fn text_keys_exist() {
    na(
        "retire-stock = \"Stop Android from starting.\"",
        "retire-stock = \"Stop Android from starting.\"\nretire-nothing = \"x\"",
        "texts.steps.retire-nothing",
    );
}

#[test]
fn no_hashes_or_unit_values() {
    let t = noteair();
    let t = format!("{t}\n# 0123456789abcdef0123456789abcdef01234567\n");
    let line = t.lines().count();
    rejects(&t, &format!("line {line}"));
}
