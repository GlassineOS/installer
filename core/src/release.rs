// SPDX-License-Identifier: GPL-2.0-or-later
//! The installer release's manifest (002 FR-019a): what the release states
//! about each artifact role it supplies: the least partition size, and the
//! file with its hash, size and kind (read by `artifacts`). How the release
//! works the numbers out is the release's rule, not the installer's. Every
//! field is optional, so a manifest of format 1 stays one.

use serde::Deserialize;
use std::collections::BTreeMap;

/// The manifest format this installer reads (and every earlier one).
pub const RELEASE_FORMAT: i64 = 1;

/// A release manifest. Keys this installer doesn't know are ignored, so a
/// release may state more than an older installer reads.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Release {
    pub release_format: i64,
    /// By artifact role (002 FR-019).
    #[serde(default)]
    pub artifacts: BTreeMap<String, Role>,
}

/// What the release states about one artifact role.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Role {
    /// The least size, in bytes, of the partition this role's artifact goes
    /// to (for `base`: the system partition).
    #[serde(default)]
    pub min_partition_bytes: Option<u64>,
    /// The artifact's file, relative to the release (a plain relative path).
    #[serde(default)]
    pub file: Option<String>,
    /// The SHA-256 of the file, lower-case hex.
    #[serde(default)]
    pub sha256: Option<String>,
    /// The file's size in bytes.
    #[serde(default)]
    pub size: Option<u64>,
    /// What the file is (`raw-image`, `btrfs-send`, …); the release's word.
    #[serde(default)]
    pub kind: Option<String>,
    /// For a kernel that starts as a kernel set (a PC, boot 010): the
    /// command line it starts with, before the installation's identities.
    #[serde(default)]
    pub cmdline: Option<String>,
    /// For a kernel: its release (boot 004's `release`).
    #[serde(default)]
    pub release: Option<String>,
}

impl Release {
    pub fn parse(json: &str) -> Result<Release, String> {
        let r: Release = serde_json::from_str(json).map_err(|e| format!("not a release manifest: {e}"))?;
        if r.release_format > RELEASE_FORMAT || r.release_format < 1 {
            return Err(format!(
                "release format {} is not one this installer reads (it reads up to {RELEASE_FORMAT})",
                r.release_format
            ));
        }
        Ok(r)
    }

    /// The least partition size the release states for `role`, if it does.
    pub fn min_partition_bytes(&self, role: &str) -> Option<u64> {
        self.artifacts.get(role).and_then(|r| r.min_partition_bytes)
    }
}
