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

fn as_json(t: &str) -> serde_json::Value {
    let v: toml::Value = toml::from_str(t).unwrap();
    serde_json::to_value(v).unwrap()
}

fn schema_accepts(t: &str) -> bool {
    let v = jsonschema::validator_for(&glassine_installer::format1::json_schema()).unwrap();
    v.is_valid(&as_json(t))
}

/// FR-004: an editor or a device repository validates with the schema alone.
#[test]
fn the_schema_accepts_the_shipped_descriptions() {
    assert!(schema_accepts(&noteair()));
    assert!(schema_accepts(&pc()));
}

#[test]
fn the_schema_rejects_what_the_structure_rejects() {
    let cases = [
        ("backend = \"android-bootloader\"", "backend = \"android-bootloader\"\ncolour = \"red\""),
        ("name = \"Onyx Boox Note Air\"\nbackend", "backend"),
        ("format = 1", "format = 2"),
        ("kind = \"write-base\"", "kind = \"dance\""),
        (
            "params = { artifact = \"kernel\", partition = \"boot_b\" }",
            "params = { artifact = \"kernel\" }",
        ),
        ("params = { artifact = \"kernel\", partition = \"boot_b\" }\nwrites", "writes"),
        (
            "equals = \"SDM EMMC\"\nrequired = true",
            "equals = \"SDM EMMC\"\none-of = [\"x\"]\nrequired = true",
        ),
        (
            "key = \"glassine.installation\"\nabsent = true",
            "key = \"glassine.installation\"\nabsent = false",
        ),
        ("class = \"resize\"", "class = \"shrink\""),
        ("select = { name = \"abl\" }", "select = { name = \"abl\", role = \"esp\" }"),
    ];
    for (old, new) in cases {
        let t = mutate(&noteair(), old, new);
        assert!(!schema_accepts(&t), "the schema accepts {new:?}");
        assert!(
            !matches!(glassine_installer::validate::load(&t), glassine_installer::validate::Loaded::Valid(_)),
            "the validator accepts {new:?}"
        );
    }
}
