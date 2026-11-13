// SPDX-License-Identifier: GPL-2.0-or-later
//! The GlassineOS installer's core: the device-description format and its
//! checks, the facts an inspection gathers, recognition, the dry-run plan and
//! the shareable report, live inspect over fastboot, the parsers a PC's
//! inspect reads its disks and firmware variables with, and the reader of the
//! RAM environment's disks (the backup's `glassine-read`) and the Sahara
//! protocol of a Qualcomm boot ROM's emergency mode. No files, processes,
//! clocks or USB here (those are the edges' business), so it builds for every
//! target the installer runs on, WebAssembly included. Everything is
//! synchronous but the fastboot and Sahara sessions and inspect, which are async only so
//! that the browser's USB can drive them; the transport is the edge's.

pub mod derive;
pub mod efivar;
pub mod facts;
pub mod fastboot;
pub mod format1;
pub mod fsprobe;
pub mod glassine_read;
pub mod inspect;
pub mod lock;
pub mod pcfacts;
pub mod plan;
pub mod ram_backup;
pub mod recognize;
pub mod release;
pub mod report;
pub mod sahara;
pub mod shape;
pub mod table;
pub mod validate;
pub mod vocab;
