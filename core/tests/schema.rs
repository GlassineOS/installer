// SPDX-License-Identifier: GPL-2.0-or-later
//! FR-004: the committed schema is the one the core generates.

mod common;
use common::*;

#[test]
fn committed_schema_is_current() {
    let got = serde_json::to_string_pretty(&glassine_installer::format1::json_schema()).unwrap() + "\n";
    golden("schema/device-description-1.schema.json", &got);
}

#[test]
fn every_vocabulary_entry_is_in_the_schema() {
    let s = serde_json::to_string(&glassine_installer::format1::json_schema()).unwrap();
    for k in glassine_installer::vocab::STEP_KINDS {
        assert!(s.contains(&format!("\"{}\"", k.id)), "{}", k.id);
    }
    for k in glassine_installer::vocab::FACT_KEYS {
        assert!(s.contains(&format!("\"{}\"", k.pattern)), "{}", k.pattern);
    }
}
