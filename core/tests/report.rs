// SPDX-License-Identifier: GPL-2.0-or-later
//! The report is an allow-list: facts full of unit-unique values give a
//! report with none of them (001 Scenario 2).

mod common;
use common::*;
use glassine_installer::facts::Snapshot;
use glassine_installer::format1::Description;
use glassine_installer::plan::{Input, plan};
use glassine_installer::recognize::recognize;
use glassine_installer::report::{render, report};

const MARK: &str = "PLANTED";

fn both(f: &Snapshot) -> String {
    let ds = [valid(&noteair()), valid(&pc())];
    let refs: Vec<&Description> = ds.iter().collect();
    let rec = recognize(&refs, f);
    let p = plan(&refs, f, &rec, &Input::default());
    let r = report(&refs, f, &rec, Some(&p));
    serde_json::to_string(&r).unwrap() + &render(&r)
}

#[test]
fn device_report_holds_no_unit_values() {
    let mut f = facts("noteair-stock-fastboot");
    f.usb.as_mut().unwrap().serial = Some(format!("{MARK}-usb-serial"));
    let v = &mut f.fastboot.as_mut().unwrap().variables;
    v.insert("serialno".into(), Some(format!("{MARK}-serialno")));
    v.insert("x-oem-secret".into(), Some(format!("{MARK}-unknown-variable")));
    v.insert("battery-voltage".into(), Some(format!("{MARK}-voltage")));
    f.hostname = Some(format!("{MARK}-host"));
    f.network = Some(glassine_installer::facts::Network {
        macs: vec![format!("{MARK}-mac")],
    });
    let out = both(&f);
    assert!(!out.contains(MARK), "{out}");
    assert!(out.contains("serialno") && out.contains("x-oem-secret"), "unknown variables are listed by name");
    assert!(out.contains("sdm660"), "allow-listed values are shown");
}

#[test]
fn android_report_holds_no_unit_values() {
    let mut f = facts("noteair-stock-adb");
    let p = &mut f.android.as_mut().unwrap().properties;
    p.insert("ro.serialno".into(), Some(format!("{MARK}-ro-serialno")));
    p.insert("ro.boot.serialno".into(), Some(format!("{MARK}-boot-serialno")));
    p.insert("persist.sys.timezone".into(), Some(format!("{MARK}-tz")));
    let out = both(&f);
    assert!(!out.contains(MARK), "{out}");
}

#[test]
fn pc_report_holds_no_unit_values() {
    let mut f = facts("pc-windows");
    let b = f.board.as_mut().unwrap();
    b.serial = Some(format!("{MARK}-board-serial"));
    b.uuid = Some(format!("{MARK}-board-uuid"));
    for e in &mut f.firmware.as_mut().unwrap().boot_entries {
        e.label = Some(format!("{MARK}-entry-label"));
        e.path = Some(format!("{MARK}-entry-path"));
    }
    for d in &mut f.storage.as_mut().unwrap().disks {
        d.guid = Some(format!("{MARK}-disk-guid"));
        d.serial = Some(format!("{MARK}-disk-serial"));
        d.model = Some(format!("{MARK}-disk-model"));
        for p in &mut d.partitions {
            p.name = Some(format!("{MARK}-label-of-a-person"));
            p.guid = Some(format!("{MARK}-part-guid"));
            if let Some(fs) = &mut p.fs {
                fs.uuid = Some(format!("{MARK}-fs-uuid"));
                fs.label = Some(format!("{MARK}-fs-label"));
            }
        }
    }
    f.systems[0].name = Some(format!("{MARK}-windows-install-name"));
    f.hostname = Some(format!("{MARK}-host"));
    f.network = Some(glassine_installer::facts::Network {
        macs: vec![format!("{MARK}-mac")],
    });
    let out = both(&f);
    assert!(!out.contains(MARK), "{out}");
    assert!(out.contains("windows"));
}

#[test]
fn unsupported_machine_report_lists_what_was_seen() {
    let f = facts("unknown-fastboot");
    let out = both(&f);
    assert!(out.contains("some-board") && out.contains("SDM UFS"));
    assert!(out.contains("\"supported\":false"));
}
