// SPDX-License-Identifier: GPL-2.0-or-later
//! The slot-flag scheme a description names (its slot-flags boot setting's
//! parameters, format 3): read for the set-boot-slot step, checked by the
//! validator.

mod common;

use common::{mutate, noteair, valid};
use glassine_installer::slot_flags::SlotFlags;
use glassine_installer::validate::{Loaded, load};

const PARAMS: &str = r#"params = { entry = "boot_{slot}", bits = "0x00ff000000000000", pinned = "0x007f000000000000", fallback = "0x007d000000000000", unbootable = "0x0080000000000000" }"#;

fn problems(t: &str) -> String {
    match load(t) {
        Loaded::Invalid(ps) => format!("{ps:?}"),
        Loaded::Valid(_) => String::new(),
        Loaded::TooNew { format } => panic!("format {format}"),
    }
}

#[test]
fn the_note_air_names_its_scheme() {
    let f = SlotFlags::of(&valid(&noteair())).unwrap().unwrap();
    assert_eq!(f.entry("b"), "boot_b");
    assert_eq!(f.slot_of("boot_a").as_deref(), Some("a"));
    assert_eq!(f.slot_of("boot"), None);
    assert_eq!(f.slot_of("boot_ab"), None);
    assert_eq!(f.slot_of("vendor_a"), None);
    assert_eq!((f.pinned, f.fallback, f.unbootable), (0x7f << 48, 0x7d << 48, 0x80 << 48));
    // only the flag bits change
    assert_eq!(f.with(0x1000_0000_0000_0001 | (0x80 << 48), f.pinned), 0x107f_0000_0000_0001);
}

#[test]
fn a_scheme_that_doesnt_read_is_refused() {
    let t = noteair();
    for (bad, why) in [
        (PARAMS.replace("boot_{slot}", "boot_b"), "{slot}"),
        (PARAMS.replace("bits = \"0x00ff000000000000\"", "bits = \"ff\""), "hex"),
        (
            PARAMS.replace("pinned = \"0x007f000000000000\"", "pinned = \"0x017f000000000000\""),
            "outside bits",
        ),
        (PARAMS.replace(", unbootable = \"0x0080000000000000\"", ""), "unbootable is missing"),
    ] {
        let p = problems(&mutate(&t, PARAMS, &bad));
        assert!(p.contains(why), "{bad}: {p}");
    }
}

#[test]
fn a_ram_slot_step_needs_the_scheme() {
    let p = problems(&mutate(&noteair(), &format!("{PARAMS}\n"), ""));
    assert!(p.contains("needs the slot-flags boot setting"), "{p}");
}
