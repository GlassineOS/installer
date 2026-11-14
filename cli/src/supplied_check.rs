// SPDX-License-Identifier: GPL-2.0-or-later
//! `supplied-check`: checks a file the person supplies for a mode (a
//! programmer for an emergency mode) against the device's description, as
//! the installer does before it sends one (spec 004 FR-004). It reads the
//! file and prints the verdict; it sends nothing, to no device and nowhere
//! else, and keeps no copy.

use glassine_installer::format1::Description;
use glassine_installer::supplied::{self, Check, Verdict};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;

/// Exit codes: a known file, a file that fits but isn't known (to be sent
/// only after the person confirms), a file that is never sent.
pub const KNOWN: u8 = 0;
pub const FITS_UNKNOWN: u8 = 2;
pub const REFUSED: u8 = 1;

pub fn cmd(args: &[String]) -> Result<ExitCode, String> {
    let o = crate::opts(args, &["--json"], &["--device", "--file", "--reported", "--devices"])?;
    let [path] = o.rest.as_slice() else {
        return Err(crate::USAGE.into());
    };
    let ds = crate::descriptions(&o)?;
    let d = pick(&ds, o.one("--device").map(String::as_str))?;
    let file = match (o.one("--file"), d.supplied_files.as_slice()) {
        (Some(f), _) => f.clone(),
        (None, [one]) => one.id.clone(),
        (None, []) => return Err(format!("{} needs no supplied file", d.id)),
        (None, many) => {
            let ids: Vec<&str> = many.iter().map(|f| f.id.as_str()).collect();
            return Err(format!("{} needs several supplied files; name one with --file: {}", d.id, ids.join(", ")));
        }
    };
    let mut reported = BTreeMap::new();
    for kv in o.all("--reported") {
        let (k, v) = kv.split_once('=').ok_or(format!("--reported takes KEY=VALUE, not {kv:?}"))?;
        reported.insert(k.to_string(), v.to_string());
    }
    let bytes = std::fs::read(Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
    let c = supplied::check(d, &file, &bytes, (!reported.is_empty()).then_some(&reported))?;
    if o.has("--json") {
        println!("{}", serde_json::to_string_pretty(&c).unwrap());
    } else {
        print!("{}", render(&c, path));
    }
    Ok(ExitCode::from(match c.verdict.sendable() {
        Some(false) => KNOWN,
        Some(true) => FITS_UNKNOWN,
        None => REFUSED,
    }))
}

/// The description by id, or the one description that has supplied files.
fn pick<'a>(ds: &'a [Description], id: Option<&str>) -> Result<&'a Description, String> {
    match id {
        Some(id) => ds.iter().find(|d| d.id == id).ok_or(format!("no description {id:?}")),
        None => {
            let with: Vec<&Description> = ds.iter().filter(|d| !d.supplied_files.is_empty()).collect();
            match with.as_slice() {
                [one] => Ok(one),
                _ => Err("name the device with --device".into()),
            }
        }
    }
}

fn pairs(m: &BTreeMap<String, String>) -> String {
    m.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ")
}

pub fn render(c: &Check, path: &str) -> String {
    let mut o = format!(
        "{path}: {} bytes, sha256 {}\nchecked as {} of {} (route {})\n",
        c.size, c.sha256, c.file, c.device, c.route
    );
    o += &match &c.verdict {
        Verdict::Known { verified } => format!(
            "known: the same as a file known to work on this device{}\n",
            if *verified { "" } else { " (seen in development, not yet in a recorded run)" }
        ),
        Verdict::FitsUnknown { values, differs } => {
            let mut s = format!(
                "a programmer, not known: it names {}, and it isn't the same as any file known to work on this device.\n",
                pairs(values)
            );
            if !differs.is_empty() {
                s += &format!(
                    "Its {} differ from the description's; this device's boot ROM runs test-signed programs, so that proves nothing either way.\n",
                    differs.join(" and ")
                );
            }
            s + "The installer sends it only after you confirm.\n"
        }
        Verdict::WrongChipOrKey {
            made_for,
            want,
            reported,
            differs,
        } => {
            let mut s = format!("refused: {} differ\n  the description: {}\n", differs.join(" and "), pairs(want));
            if let Some(dev) = reported {
                s += &format!("  the device reports: {}\n", pairs(dev));
            }
            if !made_for.is_empty() {
                s += &format!("  the file is made for: {}\n", pairs(made_for));
            }
            s
        }
        Verdict::NotAProgrammer { why } => format!("refused: not a program for this route ({why})\n"),
    };
    o + "Nothing was sent anywhere.\n"
}
