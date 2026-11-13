// SPDX-License-Identifier: GPL-2.0-or-later
//! glassine-device-files DESCRIPTION DIR
//!
//! Checks the description as the installer does (spec 002 FR-002, FR-025) and
//! writes the files derived from it (FR-030) into DIR: `device-data.list` and
//! `slots.conf`, each only when the description has something for it. Prints
//! each written file and where the installed system reads it. A device's
//! package runs this at its build. Exit 1: the description is invalid or a file
//! can't be derived; 2: usage.

use glassine_installer::derive::derive;
use glassine_installer::validate::{Loaded, load};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [desc, dir] = args.as_slice() else {
        eprintln!("usage: glassine-device-files DESCRIPTION DIR");
        return ExitCode::from(2);
    };
    let text = match std::fs::read_to_string(desc) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{desc}: {e}");
            return ExitCode::from(1);
        }
    };
    let d = match load(&text) {
        Loaded::Valid(d) => d,
        Loaded::TooNew { format } => {
            eprintln!("{desc}: format {format} is newer than this reader");
            return ExitCode::from(1);
        }
        Loaded::Invalid(ps) => {
            for p in ps {
                eprintln!("{desc}: {}: {}", if p.path.is_empty() { "(top)" } else { &p.path }, p.message);
            }
            return ExitCode::from(1);
        }
    };
    let files = match derive(&d) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{desc}: {e}");
            return ExitCode::from(1);
        }
    };
    for f in files {
        let to = std::path::Path::new(dir).join(f.file);
        if let Err(e) = std::fs::write(&to, &f.content) {
            eprintln!("{}: {e}", to.display());
            return ExitCode::from(1);
        }
        println!("{} {}", f.file, f.installed_at);
    }
    ExitCode::SUCCESS
}
