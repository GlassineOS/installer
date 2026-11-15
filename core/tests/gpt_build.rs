// SPDX-License-Identifier: GPL-2.0-or-later
//! The GPT builder against the lab's builder (the A/B table of the Note Air).
//!
//! The fixtures: `noteair-stock.gpt` is a Note Air's stock primary table
//! (MBR, header, 48 entries) with its unit-unique GUIDs replaced (the disk
//! GUID by `6c617373-696e-652d-7465-73742d646973`, the partition in slot N by
//! `67707466-6978-7475-7265-0000NNNNNNNN`) and its CRCs recomputed.
//! `noteair-ab-primary.gpt` / `noteair-ab-backup.gpt` are what the lab's own
//! builder (a separate Python program) made from it; the new partitions' GUIDs
//! in the recipe are the ones it chose. Our code never made the expected bytes.
//!
//! `GLASSINE_LAB_GPT=<dir>` with the unscrubbed `gpt_main0.bin`,
//! `gpt_main_new.bin`, `gpt_backup_new.bin` and `ab_layout.json` runs the
//! same build on the real table (the bytes that went to the device).

mod common;
use common::root;
use glassine_installer::gpt_build::{Change, Recipe, build};
use glassine_installer::lock::sha256_hex;

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(root().join("core/tests/fixtures/gpt").join(name)).unwrap()
}

fn recipe() -> Recipe {
    serde_json::from_slice(&fixture("noteair-ab.recipe.json")).unwrap()
}

const DISK: u64 = 61_071_360 * 512;

#[test]
fn the_note_air_ab_table_equals_the_lab_builders_bytes() {
    let b = build(&fixture("noteair-stock.gpt"), Some(DISK), &recipe()).unwrap();
    assert_eq!(b.primary, fixture("noteair-ab-primary.gpt"));
    assert_eq!(b.backup, fixture("noteair-ab-backup.gpt"));
    assert_eq!(b.backup_sector, 61_071_343);
    assert_eq!(b.backup_offset() + b.backup.len() as u64, DISK, "the backup ends on the last sector");
    let lines = b.describe();
    assert_eq!(lines.len(), 8, "{lines:#?}");
    assert!(lines[0].contains("boot is renamed boot_a") && lines[0].contains("0x007f000000000000"));
    assert!(matches!(
        b.changes[3],
        Change::Shrunk {
            slot: 48,
            last_before: 61_071_326,
            last_after: 53_009_407,
            ..
        }
    ));
    let added: Vec<(u32, u64, u64)> = b
        .changes
        .iter()
        .filter_map(|c| match c {
            Change::Added { slot, first, last, .. } => Some((*slot, *first, *last)),
            _ => None,
        })
        .collect();
    // The positions the layout document derived (slots 49..51 = p49..p51).
    assert_eq!(
        added,
        vec![(49, 53_009_408, 53_140_479), (50, 53_140_480, 59_431_935), (51, 59_431_936, 61_070_335)]
    );
    assert!(matches!(
        b.changes[7],
        Change::EntryArray {
            entries_before: 48,
            entries_after: 64,
            last_usable_after: 61_071_342,
            ..
        }
    ));
}

#[test]
fn the_real_table_from_the_lab_byte_for_byte() {
    let Ok(dir) = std::env::var("GLASSINE_LAB_GPT") else {
        eprintln!("GLASSINE_LAB_GPT not set: skipped");
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    let read = |n: &str| std::fs::read(dir.join(n)).unwrap_or_else(|e| panic!("{n}: {e}"));
    let layout: serde_json::Value = serde_json::from_slice(&read("ab_layout.json")).unwrap();
    let mut r = recipe();
    for n in &mut r.new {
        let p = layout["partitions"].as_array().unwrap().iter().find(|p| p["name"] == n.name.as_str()).unwrap();
        n.guid = p["partuuid"].as_str().unwrap().to_string();
    }
    let b = build(&read("gpt_main0.bin"), Some(DISK), &r).unwrap();
    assert_eq!(sha256_hex(&b.primary), "268f0f35ad6a247557f23bac73b9f26f980b32f436f3aec326241916a610e72e");
    assert_eq!(sha256_hex(&b.backup), "6215658aadb2771696bba3226d6fe1f0816e0fa719413821275368fb9ce6d481");
    assert_eq!(b.primary, read("gpt_main_new.bin"));
    assert_eq!(b.backup, read("gpt_backup_new.bin"));
}

fn refused(recipe: &Recipe) -> String {
    build(&fixture("noteair-stock.gpt"), Some(DISK), recipe).unwrap_err()
}

#[test]
fn a_damaged_or_foreign_source_is_refused() {
    let mut src = fixture("noteair-stock.gpt");
    src[1024 + 60] ^= 1; // an entry byte: the array CRC no longer matches
    assert!(build(&src, None, &recipe()).unwrap_err().contains("entry array CRC"));
    let mut src = fixture("noteair-stock.gpt");
    src[512 + 40] ^= 1; // the header
    assert!(build(&src, None, &recipe()).unwrap_err().contains("header CRC"));
    let e = build(&fixture("noteair-stock.gpt"), Some(DISK - 512 * 2048), &recipe()).unwrap_err();
    assert!(e.contains("not this disk's table"), "{e}");
}

#[test]
fn a_recipe_naming_a_missing_or_unknown_partition_is_refused() {
    let mut r = recipe();
    r.renames[1].from = "systen".into();
    assert!(refused(&r).contains("no partition systen"));
    let mut r = recipe();
    r.renames.push(r.renames[1].clone());
    assert!(refused(&r).contains("renamed twice"));
    let mut r = recipe();
    r.renames[2].to = "recovery".into();
    assert!(refused(&r).contains("two partitions would be named recovery"));
}

#[test]
fn shrinking_anything_but_the_last_partition_is_refused() {
    // Shrinking boot would leave userdata where the new partitions go.
    let mut r = recipe();
    r.shrink = Some("system".into());
    let e = refused(&r);
    assert!(e.contains("wouldn't shrink") || e.contains("overlap"), "{e}");
    let mut r = recipe();
    r.shrink = None;
    let e = refused(&r);
    assert!(e.contains("need a partition that shrinks"), "{e}");
}

#[test]
fn an_unaligned_or_oversized_new_partition_is_refused() {
    let mut r = recipe();
    r.new[0].size = glassine_installer::gpt_build::Size::Bytes(512 * 1000);
    assert!(refused(&r).contains("not a multiple of the alignment"));
    let mut r = recipe();
    r.new[1].size = glassine_installer::gpt_build::Size::Bytes(25 << 30);
    assert!(refused(&r).contains("too small to give up"));
}

#[test]
fn growing_the_array_past_the_first_usable_sector_is_refused() {
    let mut r = recipe();
    r.entries = 512; // 128 sectors of entries; the first usable sector is 34
    assert!(refused(&r).contains("past the first usable sector"));
    r.entries = 40;
    assert!(refused(&r).contains("fewer than the source's 48"));
}

#[test]
fn every_slot_the_recipe_does_not_name_is_byte_identical() {
    let src = fixture("noteair-stock.gpt");
    let b = build(&src, Some(DISK), &recipe()).unwrap();
    let named = [8usize, 10, 11, 48]; // boot, system, vendor, userdata (1-based)
    for i in 0..48 {
        let a = &src[1024 + i * 128..1024 + (i + 1) * 128];
        let o = &b.primary[1024 + i * 128..1024 + (i + 1) * 128];
        if named.contains(&(i + 1)) {
            // Renamed/shrunk ones keep type, GUID and start.
            assert_eq!(a[0..40], o[0..40], "slot {}", i + 1);
        } else {
            assert_eq!(a, o, "slot {}", i + 1);
        }
    }
    assert_eq!(b.primary[0..512], src[0..512], "the protective MBR is kept");
}
