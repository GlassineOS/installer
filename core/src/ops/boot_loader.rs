// SPDX-License-Identifier: GPL-2.0-or-later
//! `write-boot-loader`: the boot loader artifact over the boot loader
//! partition, through the running boot loader's fastboot (the stock one on a
//! device in its stock state). The same write as [`super::write_image::flash`];
//! whether it took is the step's `done` condition, read after the device
//! restarted into the new boot loader (a boot loader that comes back as the
//! old one fails the step).

use super::{Evidence, OpError};
use crate::artifacts::{ArtifactSource, Artifacts};
use crate::fastboot::{Session, Transport, WriteRules};

pub async fn run<T: Transport, S: ArtifactSource>(
    s: &mut Session<T>,
    rules: &WriteRules,
    part: &str,
    role: &str,
    artifacts: &mut Artifacts<S>,
) -> Result<Evidence, OpError> {
    let mut ev = super::write_image::flash(s, rules, part, role, artifacts).await?;
    ev.insert("product-before".into(), s.product().unwrap_or("").into());
    Ok(ev)
}
