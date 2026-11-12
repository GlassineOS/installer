// SPDX-License-Identifier: GPL-2.0-or-later
//! Pinned copies (FR-027, FR-028): the lock file lists, per shipped copy of a
//! description, its repository, the version and the content's hash. The
//! checks fail when a copy differs from its entry, an entry has no copy, a
//! copy has no entry, or a copy fails the format's checks.

use crate::shape::{Problem, problem};
use crate::validate::{Loaded, load};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const LOCK_FORMAT: i64 = 1;
/// The version of a description this repository owns itself.
pub const OWN: &str = "this-repository";
/// A copy whose repository doesn't hold the description yet.
pub const UNPINNED: &str = "unpinned";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lock {
    pub lock_format: i64,
    #[serde(default)]
    pub copies: Vec<Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// The copy, relative to the lock file.
    pub file: String,
    /// `owner/name` of the repository the description lives in.
    pub repository: String,
    /// The path of the description in that repository.
    pub path: String,
    /// A tag or commit of that repository, `this-repository`, or `unpinned`.
    pub version: String,
    pub sha256: String,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// The result of checking a lock file against the copies next to it.
#[derive(Debug, Default)]
pub struct Checked {
    pub problems: Vec<Problem>,
    /// Entries whose repository doesn't pin them yet: `(repository, path)`.
    pub unpinned: Vec<(String, String)>,
}

/// `copies` are the description files next to the lock file, by file name.
/// `require_pins` turns an unpinned entry into a failure (once its repository
/// holds the description).
pub fn check(lock_text: &str, own_repository: &str, copies: &[(String, Vec<u8>)], require_pins: bool) -> Checked {
    let mut out = Checked::default();
    let lock: Lock = match toml::from_str(lock_text) {
        Ok(l) => l,
        Err(e) => {
            out.problems.push(problem("lock", format!("not a lock file: {}", e.message())));
            return out;
        }
    };
    if lock.lock_format != LOCK_FORMAT {
        out.problems.push(problem("lock_format", format!("unknown lock format {}", lock.lock_format)));
        return out;
    }
    for (i, e) in lock.copies.iter().enumerate() {
        let path = format!("copies[{i}]");
        if lock.copies.iter().take(i).any(|x| x.file == e.file) {
            out.problems.push(problem(&format!("{path}.file"), format!("{} has two entries", e.file)));
        }
        let Some((_, bytes)) = copies.iter().find(|(n, _)| *n == e.file) else {
            out.problems
                .push(problem(&format!("{path}.file"), format!("no copy {} next to the lock file", e.file)));
            continue;
        };
        if sha256_hex(bytes) != e.sha256 {
            out.problems.push(problem(
                &format!("{path}.sha256"),
                format!("{} differs from the version it is pinned to", e.file),
            ));
        }
        match (e.repository == own_repository, e.version.as_str()) {
            (true, OWN) => {}
            (true, _) => out.problems.push(problem(
                &format!("{path}.version"),
                format!("a description of {own_repository} is pinned as {OWN}"),
            )),
            (false, OWN) => out.problems.push(problem(
                &format!("{path}.version"),
                format!("only {own_repository}'s own descriptions are {OWN}"),
            )),
            (false, UNPINNED) => {
                out.unpinned.push((e.repository.clone(), e.path.clone()));
                if require_pins {
                    out.problems.push(problem(
                        &format!("{path}.version"),
                        format!("{} now has the description: pin a version", e.repository),
                    ));
                }
            }
            (false, v) if v.trim().is_empty() => out.problems.push(problem(&format!("{path}.version"), "is empty")),
            _ => {}
        }
        match load(&String::from_utf8_lossy(bytes)) {
            Loaded::Valid(_) => {}
            Loaded::TooNew { format } => out
                .problems
                .push(problem(&path, format!("{} is format {format}, newer than this installer reads", e.file))),
            Loaded::Invalid(ps) => {
                for p in ps {
                    out.problems.push(problem(&format!("{}:{}", e.file, p.path), p.message));
                }
            }
        }
    }
    for (name, _) in copies {
        if !lock.copies.iter().any(|e| e.file == *name) {
            out.problems.push(problem("copies", format!("{name} has no lock entry")));
        }
    }
    out
}
