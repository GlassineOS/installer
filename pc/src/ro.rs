// SPDX-License-Identifier: GPL-2.0-or-later
//! The one place this crate opens a file: read-only (`File::open` is
//! `O_RDONLY`), and the file never leaves this module. What leaves is a
//! [`Reader`], which can only read at an offset. Opening a disk read-only
//! changes nothing on it.

use glassine_installer::table::ReadAt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub struct Reader {
    file: File,
    path: String,
}

#[allow(clippy::disallowed_methods)] // the one read-only open
pub fn open(path: &Path) -> Result<Reader, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Reader {
        file,
        path: path.display().to_string(),
    })
}

impl ReadAt for Reader {
    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        self.file
            .seek(SeekFrom::Start(offset))
            .and_then(|_| self.file.read_exact(buf))
            .map_err(|e| format!("{}: read of {} bytes at {offset}: {e}", self.path, buf.len()))
    }
}
