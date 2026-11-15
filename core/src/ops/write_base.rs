// SPDX-License-Identifier: GPL-2.0-or-later
//! `write-base`: the base and the first deployment on the system partition.
//!
//! Through the RAM environment the base comes as a pool image the release
//! builds (kind `btrfs-pool-image`: a btrfs file system smaller than the
//! partition, laid out by `glassine-deploy init`, holding the received base
//! and the install marker; GlassineOS grows it to the partition at its first
//! start). It is written like [`super::write_image::write_ram`]: only its
//! length, each WRITE read back, then the device's hash of every range. A base
//! of any other kind isn't written this way.

use super::{Evidence, Gpt, OpError};
use crate::artifacts::{ArtifactSource, Artifacts};
use crate::fastboot::Transport;
use crate::glassine_write::Writer;

/// The kind of base this step writes through range writes.
pub const POOL_IMAGE: &str = "btrfs-pool-image";

#[allow(clippy::too_many_arguments)]
pub async fn run<T: Transport, S: ArtifactSource>(
    w: &mut Writer<T>,
    disk: u32,
    gpt: &Gpt,
    part: &str,
    role: &str,
    artifacts: &mut Artifacts<S>,
    max_hash: u64,
    progress: &mut dyn FnMut(u64, u64),
) -> Result<Evidence, OpError> {
    let a = artifacts.artifact(role).map_err(|e| OpError::before(e.to_string()))?;
    if a.kind.as_deref() != Some(POOL_IMAGE) {
        return Err(OpError::before(format!(
            "the release's {role} is of kind {:?}; through the RAM environment only a {POOL_IMAGE} is written",
            a.kind.as_deref().unwrap_or("(none)")
        )));
    }
    let mut ev = super::write_image::write_ram(w, disk, gpt, part, role, artifacts, max_hash, progress).await?;
    ev.insert("kind".into(), POOL_IMAGE.into());
    Ok(ev)
}
