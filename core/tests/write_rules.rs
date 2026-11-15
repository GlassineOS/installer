// SPDX-License-Identifier: GPL-2.0-or-later
//! The fastboot write rules derived from a description's mode (002 FR-011,
//! FR-014, FR-022): checked before anything is sent.

mod common;
use common::*;
use glassine_installer::fastboot::{FbError, Framing};
use glassine_installer::write_rules::{ModeRules, mode_rules};

fn rules(mode: &str) -> ModeRules {
    mode_rules(&valid(&noteair()), mode, "install").unwrap()
}

fn refused(r: &ModeRules, part: &str) -> String {
    match r.check(part) {
        Err(FbError::RefusedWrite(w)) => w,
        other => panic!("{part}: {other:?}"),
    }
}

#[test]
fn the_stock_boot_loader_writes_only_the_boot_loader() {
    let r = rules("fastboot-stock");
    assert_eq!(r.rules.framing, Framing::EqualLength { len: 65535 });
    assert_eq!(r.writes, vec!["abl".to_string()]);
    assert!(r.rules.oem.is_empty(), "the stock boot loader has no oem-reboot-recovery");
    assert!(r.check_oem("reboot-recovery").is_err());
    r.check("abl").unwrap();
    assert!(refused(&r, "recovery").contains("no step of the installation writes recovery"));
    for n in ["devinfo", "boot", "boot_a", "vendor", "misc", "onyxconfig"] {
        assert!(refused(&r, n).contains("never written"), "{n}");
    }
}

#[test]
fn the_glassine_boot_loader_writes_only_the_recovery_and_may_restart_into_it() {
    let r = rules("fastboot");
    assert_eq!(r.rules.framing, Framing::EqualLength { len: 65535 });
    assert_eq!(r.writes, vec!["recovery".to_string()]);
    assert_eq!(r.rules.oem, vec!["reboot-recovery".to_string()]);
    r.check_oem("reboot-recovery").unwrap();
    assert!(r.check_oem("unlock").is_err());
    r.check("recovery").unwrap();
    // boot_a may be written in the RAM environment (keep-stock), never over fastboot
    assert!(refused(&r, "boot_a").contains("never written"));
    assert!(refused(&r, "abl").contains("no step"));
}

#[test]
fn the_partition_table_is_never_flashed() {
    for m in ["fastboot-stock", "fastboot"] {
        let r = rules(m);
        assert!(
            r.check_operation("flash-partition-table")
                .unwrap_err()
                .to_string()
                .contains("erases the whole storage")
        );
        for n in ["partition", "partition:0", "PARTITION"] {
            assert!(refused(&r, n).contains("partition table"), "{m} {n}");
        }
    }
}

/// abl's flash of a system partition switches verified boot to enforcing
/// first, which writes devinfo: refused whatever the description allows.
#[test]
fn system_partitions_are_never_written_over_fastboot() {
    let t = mutate(
        &noteair(),
        "params = { artifact = \"ram-environment\", partition = \"recovery\" }\nwrites = [\"recovery\"]",
        "params = { artifact = \"ram-environment\", partition = \"system_b\" }\nwrites = [\"system_b\"]",
    );
    let r = mode_rules(&valid(&t), "fastboot", "install").unwrap();
    assert_eq!(r.writes, vec!["system_b".to_string()]);
    for n in ["system_b", "system", "System_a", "systemrw"] {
        assert!(refused(&r, n).contains("devinfo"), "{n}");
    }
}

#[test]
fn a_mode_that_marks_flash_never_use_writes_nothing() {
    let t = mutate(
        &noteair(),
        "params = { operation = \"flash-partition-table\", reason = \"the boot loader erases the whole storage before it writes the new partition table: every partition would be lost, those GlassineOS never writes too\" }\nuntried = \"from the boot loaders' source (WriteGpt erases the whole storage before the table is written); never sent\"\n\n[[modes.departures]]\nkind = \"oem-reboot-recovery\"",
        "params = { operation = \"flash\", reason = \"no\" }\nuntried = \"x\"\n\n[[modes.departures]]\nkind = \"oem-reboot-recovery\"",
    );
    let r = mode_rules(&valid(&t), "fastboot", "install").unwrap();
    assert!(refused(&r, "recovery").contains("never to use flash"));
}

#[test]
fn only_fastboot_modes_have_rules() {
    let d = valid(&noteair());
    assert!(mode_rules(&d, "ram", "install").unwrap_err().contains("not fastboot"));
    assert!(mode_rules(&d, "nope", "install").is_err());
}
