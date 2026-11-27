// SPDX-License-Identifier: GPL-2.0-or-later
//! Spec 002 FR-018, FR-030: the installed system's device files derived from
//! the description, and the checks of the `[[device_data]]` entries they come from.

mod common;
use common::*;
use glassine_installer::derive::{deploy_conf, derive, device_data_list, kept_system_slot, slots_conf, system_partitions};
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
            ("slots.conf", "/etc/glassine/slots.conf"),
            ("deploy.conf.device", "/usr/share/glassine/deploy.conf.device"),
        ]
    );
    golden("core/tests/golden/onyx-noteair.device-data.list", &files[0].content);
    golden("core/tests/golden/onyx-noteair.slots.conf", &files[1].content);
    golden("core/tests/golden/onyx-noteair.deploy.conf.device", &files[2].content);
}

/// FR-018c: the facts glassine-deploy reads (GlassineOS/boot conf/deploy.conf.example),
/// as the lines the device's hand-written deploy.conf had, and the space Android's
/// encryption footer needs.
#[test]
fn the_note_airs_deploy_facts() {
    let d = valid(&noteair());
    assert_eq!(system_partitions(&d), ["system_b"]);
    let c = deploy_conf(&d).unwrap().unwrap();
    assert_eq!(
        entries(&c),
        [
            ["SYSTEM_DEV=/dev/disk/by-partlabel/system_b"],
            ["USERDATA_DEV=/dev/disk/by-partlabel/userdata"],
            ["USERDATA_RESERVE_KIB=16"],
            ["KEEP_UPDATES=1"],
        ]
    );
}

/// FR-018c, boot spec 007 FR-002: KEEP_UPDATES from keep_updates; 1 when absent.
#[test]
fn keep_updates_is_derived() {
    let t = mutate(&noteair(), "keep_updates = 1", "keep_updates = 4");
    let c = deploy_conf(&valid(&t)).unwrap().unwrap();
    assert!(c.lines().any(|l| l == "KEEP_UPDATES=4"), "{c}");
    let t = mutate(&noteair(), "keep_updates = 1\n", "");
    let c = deploy_conf(&valid(&t)).unwrap().unwrap();
    assert!(c.lines().any(|l| l == "KEEP_UPDATES=1"), "{c}");
}

#[test]
fn no_kept_free_space_is_zero() {
    let t = mutate(&noteair(), "keep_free_at_end = \"16KiB\"\n", "");
    let c = deploy_conf(&valid(&t)).unwrap().unwrap();
    assert!(c.lines().any(|l| l == "USERDATA_RESERVE_KIB=0"), "{c}");
    let t = mutate(&noteair(), "keep_free_at_end = \"16KiB\"", "keep_free_at_end = \"2MiB\"");
    let c = deploy_conf(&valid(&t)).unwrap().unwrap();
    assert!(c.lines().any(|l| l == "USERDATA_RESERVE_KIB=2048"), "{c}");
}

#[test]
fn no_data_partition_no_deploy_conf() {
    let t = mutate(
        &noteair(),
        "[data_partition]\nname = \"userdata\"\nkeep_free_at_end = \"16KiB\"\nkeep_updates = 1\n",
        "",
    );
    let d = valid(&t);
    assert_eq!(deploy_conf(&d).unwrap(), None);
    assert!(derive(&d).unwrap().iter().all(|f| f.file != "deploy.conf.device"));
}

#[test]
fn a_data_partition_without_one_system_partition() {
    let mut d = valid(&noteair());
    let mut s = d.step("partition-table").unwrap().clone();
    s.id = "partition-table-2".into();
    let p: toml::Table = "partitions = [{ name = \"system_c\", role = \"system\" }]".parse().unwrap();
    s.params.extend(p);
    d.steps.push(s);
    assert!(deploy_conf(&d).unwrap_err().contains("ambiguous"));
    let mut d = valid(&noteair());
    d.steps.iter_mut().for_each(|s| {
        s.params.remove("partitions");
    });
    assert!(deploy_conf(&d).unwrap_err().contains("no step creates"));
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

/// A PC names a data partition, but its partitions are found by the IDs on the
/// command line, not by names: its deploy.conf.device sets neither SYSTEM_DEV nor
/// USERDATA_DEV (glassine-deploy leaves both empty and reads the command line), only
/// the space kept free and how many earlier deployments stay. Nothing else is derived.
#[test]
fn a_pc_derives_its_deploy_facts_only() {
    let d = valid(&pc());
    assert_eq!(d.data_partition.as_ref().map(|p| p.name.as_str()), Some("glassine-userdata"));
    let files = derive(&d).unwrap();
    let names: Vec<_> = files.iter().map(|f| (f.file, f.installed_at)).collect();
    assert_eq!(names, [("deploy.conf.device", "/usr/share/glassine/deploy.conf.device")]);
    golden("core/tests/golden/uefi-pc.deploy.conf.device", &files[0].content);
    assert_eq!(entries(&files[0].content), [["USERDATA_RESERVE_KIB=0"], ["KEEP_UPDATES=4"]]);
    assert!(!files[0].content.contains("by-partlabel"), "{}", files[0].content);
}

/// The PC's keep_updates and keep_free_at_end go through as the Note Air's do.
#[test]
fn a_pcs_deploy_facts_follow_its_description() {
    let t = mutate(&pc(), "keep_updates = 4", "keep_updates = 2\nkeep_free_at_end = \"1MiB\"");
    let c = deploy_conf(&valid(&t)).unwrap().unwrap();
    assert_eq!(entries(&c), [["USERDATA_RESERVE_KIB=1024"], ["KEEP_UPDATES=2"]]);
    let t = mutate(&pc(), "keep_updates = 4\n", "");
    let c = deploy_conf(&valid(&t)).unwrap().unwrap();
    assert!(c.lines().any(|l| l == "KEEP_UPDATES=1"), "{c}");
}

/// The PC's derived lines read as glassine-deploy sources its configuration: a key
/// the file doesn't set keeps glassine-deploy's default, so SYSTEM_DEV and
/// USERDATA_DEV stay empty for the command line to fill.
#[test]
fn a_pcs_deploy_facts_leave_the_partitions_empty() {
    let c = deploy_conf(&valid(&pc())).unwrap().unwrap();
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(
            "SYSTEM_DEV='' USERDATA_DEV='' USERDATA_RESERVE_KIB='' KEEP_UPDATES=1; eval \"$1\"; \
              printf '%s|%s|%s|%s' \"$SYSTEM_DEV\" \"$USERDATA_DEV\" \"$USERDATA_RESERVE_KIB\" \"$KEEP_UPDATES\"",
        )
        .arg("sh")
        .arg(&c)
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "||0|4");
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
