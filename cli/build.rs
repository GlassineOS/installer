// SPDX-License-Identifier: GPL-2.0-or-later
//! Embeds the shipped description copies (devices/*.toml) so the program
//! carries them; the code names no device.
use std::{env, fs, path::Path};

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../devices");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut names: Vec<_> = fs::read_dir(&dir)
        .map(|r| r.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    names.retain(|p: &std::path::PathBuf| p.extension().is_some_and(|x| x == "toml") && p.file_name().is_some_and(|n| n != "lock.toml"));
    names.sort();
    let mut out = String::from("pub const EMBEDDED: &[(&str, &str)] = &[\n");
    for p in names {
        println!("cargo:rerun-if-changed={}", p.display());
        let abs = fs::canonicalize(&p).unwrap();
        out.push_str(&format!(
            "    ({:?}, include_str!({:?})),\n",
            p.file_name().unwrap().to_string_lossy(),
            abs.display().to_string()
        ));
    }
    out.push_str("];\n");
    fs::write(Path::new(&env::var("OUT_DIR").unwrap()).join("embedded.rs"), out).unwrap();
}
