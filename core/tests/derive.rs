// SPDX-License-Identifier: GPL-2.0-or-later
//! Spec 002 FR-018, FR-030: the installed system's device files derived from
//! the description, and the checks of the `[[device_data]]` entries they come from.

mod common;
use common::*;
use glassine_installer::derive::{derive, device_data_list, kept_system_slot, slots_conf};
use glassine_installer::validate::{Loaded, load};

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

/// The lines that aren't comments, as their fields.
fn entries(t: &str) -> Vec<Vec<String>> {
    t.lines()
        .filter(|l| !l.trim_start().starts_with('#') && !l.trim().is_empty())
        .map(|l| l.split_whitespace().map(String::from).collect())
        .collect()
}

#[test]
fn the_note_airs_files_are_golden() {
    let d = valid(&noteair());
    let files = derive(&d).unwrap();
    let names: Vec<_> = files.iter().map(|f| (f.file, f.installed_at)).collect();
    assert_eq!(
        names,
        [
            ("device-data.list", "/usr/share/glassine/device-data.list"),
            ("slots.conf", "/etc/glassine/slots.conf")
        ]
    );
    golden("core/tests/golden/onyx-noteair.device-data.list", &files[0].content);
    golden("core/tests/golden/onyx-noteair.slots.conf", &files[1].content);
}

/// The facts the hand-written files of device-onyx-noteair held (GlassineOS/packages
/// main at 189ca4e, before they were derived) are the description's now, line for
/// line. The bytes differ only in comments (the header says the file is derived;
/// each group's comment is its `what`, wrapped at 88 columns) and in the CHECK
/// column, which follows the longest source instead of a hand-picked column.
#[test]
fn the_derived_files_say_what_the_hand_written_ones_did() {
    let d = valid(&noteair());
    let list = device_data_list(&d).unwrap().unwrap();
    let was = text("core/tests/fixtures/derived/onyx-noteair-handwritten.device-data.list");
    assert_eq!(entries(&list), entries(&was));
    let slots = slots_conf(&d).unwrap().unwrap();
    let was = text("core/tests/fixtures/derived/onyx-noteair-handwritten.slots.conf");
    assert_eq!(entries(&slots), entries(&was));
    assert_eq!(kept_system_slot(&d).unwrap().as_deref(), Some("a"));
}

#[test]
fn a_pc_derives_nothing() {
    assert!(derive(&valid(&pc())).unwrap().is_empty());
}

#[test]
fn no_kept_system_no_slots_conf() {
    let t = mutate(&noteair(), "params = { slot = \"a\", unbootable = true }", "params = { slot = \"a\" }");
    assert_eq!(slots_conf(&valid(&t)).unwrap(), None);
}

#[test]
fn two_retired_slots_are_ambiguous() {
    let mut d = valid(&noteair());
    let mut s = d.step("retire-stock").unwrap().clone();
    s.id = "retire-stock-2".into();
    s.params.insert("slot".into(), toml::Value::String("b".into()));
    d.steps.push(s);
    assert!(kept_system_slot(&d).unwrap_err().contains("ambiguous"));
}

#[test]
fn format_1_names_derive_no_list() {
    let d = valid(&text("core/tests/fixtures/descriptions/onyx-noteair-format1.toml"));
    assert!(device_data_list(&d).unwrap_err().contains("only a name"));
}

#[test]
fn an_entry_without_a_check_has_no_check_column() {
    let t = mutate(&noteair(), "check = \"wbf\"\n", "");
    let list = device_data_list(&valid(&t)).unwrap().unwrap();
    assert!(list.lines().all(|l| !l.ends_with(' ')));
    assert!(!list.contains("CHECK"));
}

// ---- FR-018: what a device-data entry may say -------------------------------------

#[test]
fn a_format_2_entry_is_a_table() {
    let t = mutate(&pc(), "generic = true\n", "generic = true\ndevice_data = [\"gpu-firmware\"]\n");
    rejects(&t, "device_data[0]");
}

#[test]
fn a_format_1_entry_is_a_name() {
    let t = text("core/tests/fixtures/descriptions/onyx-noteair-format1.toml");
    let t = mutate(
        &t,
        "device_data = [\"gpu-firmware\", \"sensor-calibration\", \"modem-firmware\", \"wifi-firmware\"]",
        "device_data = [{ name = \"a\", partition = \"modem\", path = \"/a\", what = \"a\" }]",
    );
    rejects(&t, "device_data[0]");
}

#[test]
fn an_entry_needs_its_keys() {
    na("name = \"waveform.wbf\"\n", "", "device_data[0].name");
    na("path = \"/sensors/sns.reg\"\n", "", "device_data[12].path");
    na("what = \"display waveform\"\n", "", "device_data[0].what");
}

#[test]
fn an_unknown_check() {
    na("check = \"wbf\"", "check = \"crc\"", "device_data[0].check");
}

#[test]
fn a_name_twice() {
    na(
        "name = \"firmware/qca/crnv21.bin\"",
        "name = \"firmware/qca/crbtfw21.tlv\"",
        "device_data[7].name",
    );
}

#[test]
fn a_name_not_relative_or_with_dot_parts() {
    na("name = \"waveform.wbf\"", "name = \"/waveform.wbf\"", "device_data[0].name");
    na("name = \"waveform.wbf\"", "name = \"../waveform.wbf\"", "device_data[0].name");
    na("name = \"waveform.wbf\"", "name = \"a/./waveform.wbf\"", "device_data[0].name");
    na("name = \"waveform.wbf\"", "name = \"manifest\"", "device_data[0].name");
}

#[test]
fn a_path_not_absolute_or_with_dot_parts() {
    na(
        "path = \"/waveform/eink_waveform.wbf\"",
        "path = \"waveform/eink_waveform.wbf\"",
        "device_data[0].path",
    );
    na(
        "path = \"/waveform/eink_waveform.wbf\"",
        "path = \"/waveform/../eink_waveform.wbf\"",
        "device_data[0].path",
    );
}

#[test]
fn a_pattern_on_one_side_only_or_not_last() {
    na("path = \"/image/adsp.*\"", "path = \"/image/adsp.mdt\"", "device_data[3].path");
    na(
        "name = \"firmware/qcom/sdm636/onyx/noteair/mba.mbn\"",
        "name = \"firmware/*/mba.mbn\"",
        "device_data[1].name",
    );
}

#[test]
fn a_partition_no_rule_knows() {
    na("partition = \"persist\"", "partition = \"nowhere\"", "device_data[12].partition");
}

/// Device data is only ever read: it never makes a partition a job may change its
/// source, and naming one relaxes nothing (a never-write partition stays one).
#[test]
fn a_partition_a_job_changes() {
    na("partition = \"persist\"", "partition = \"userdata\"", "device_data[12].partition");
    na("partition = \"persist\"", "partition = \"system_b\"", "device_data[12].partition");
    na("partition = \"persist\"", "partition = \"abl\"", "device_data[12].partition");
}
