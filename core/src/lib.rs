// SPDX-License-Identifier: GPL-2.0-or-later
//! The GlassineOS installer's core: the device-description format and its
//! checks, the facts an inspection gathers, recognition, the dry-run plan and
//! the shareable report. Pure and synchronous: no files, processes, clocks or
//! USB here (those are the edges' business), so it builds for every target the
//! installer runs on, WebAssembly included.

pub mod facts;
pub mod format1;
pub mod lock;
pub mod plan;
pub mod recognize;
pub mod report;
pub mod shape;
pub mod validate;
pub mod vocab;
