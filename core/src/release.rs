// SPDX-License-Identifier: GPL-2.0-or-later
//! The installer release's manifest (002 FR-019a): what the release states
//! about each artifact role it supplies. The core reads only the numbers; how
//! the release works them out is the release's rule, not the installer's.

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
