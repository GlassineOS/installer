// SPDX-License-Identifier: GPL-2.0-or-later
//! The GlassineOS installer's core: the device-description format and its
//! checks, the facts an inspection gathers, recognition, the dry-run plan and
//! the shareable report, live inspect over fastboot, and the reader of the RAM
//! environment's disks (the backup's `glassine-read`). No files, processes,
//! clocks or USB here (those are the edges' business), so it builds for every
//! target the installer runs on, WebAssembly included. Everything is
//! synchronous but the fastboot session and inspect, which are async only so
//! that the browser's USB can drive them; the transport is the edge's.

pub mod facts;
pub mod fastboot;
pub mod format1;
pub mod glassine_read;
pub mod inspect;
pub mod lock;
pub mod plan;
pub mod recognize;
pub mod report;
pub mod shape;
pub mod validate;
pub mod vocab;
