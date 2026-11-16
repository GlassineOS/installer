// SPDX-License-Identifier: GPL-2.0-or-later
//! The PC executor's edge on Linux: the record in a file (a whole copy under
//! a temporary name, synced, then renamed), what is kept before the first
//! write in `RECORD.kept/`, and the backup in a folder, each disk's image
//! gzip-compressed (`disk<N>.img.gz`), every file synced before it counts.

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use glassine_installer::pc_execute::PcEdge;
use glassine_installer::record::Record;
use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};

pub struct DirEdge {
    pub record: PathBuf,
    pub kept: PathBuf,
    pub backup: Option<PathBuf>,
    /// a file whose presence asks to stop after the next safe step
    pub cancel_file: Option<PathBuf>,
    open: Option<(PathBuf, GzEncoder<BufWriter<File>>)>,
    last: (String, u64),
}

fn plain(name: &str) -> Result<(), String> {
    if name.is_empty() || name.contains('/') || name.starts_with('.') {
        return Err(format!("{name:?} isn't a plain file name"));
    }
    Ok(())
}

fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

/// Writes `bytes` to `path` whole: under a temporary name, synced, renamed.
fn write_whole(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut t = path.as_os_str().to_owned();
    t.push(".partial");
    let t = PathBuf::from(t);
    let mut f = File::create(&t).map_err(|e| format!("{}: {e}", t.display()))?;
    f.write_all(bytes).and_then(|_| f.sync_all()).map_err(|e| format!("{}: {e}", t.display()))?;
    std::fs::rename(&t, path).map_err(|e| format!("{}: {e}", path.display()))?;
    // The renamed file synced again: on FAT the rename leaves its directory
    // entry to the file's next write-back, which a power cut can lose.
    File::open(path).and_then(|f| f.sync_all()).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(d) = path.parent() {
        sync_dir(d);
    }
    Ok(())
}

fn read_opt(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match std::fs::read(path) {
        Ok(b) => Ok(Some(b)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

impl DirEdge {
    pub fn new(record: &Path, backup: Option<PathBuf>, cancel_file: Option<PathBuf>) -> DirEdge {
        let mut k = record.as_os_str().to_owned();
        k.push(".kept");
        DirEdge {
            record: record.to_path_buf(),
            kept: PathBuf::from(k),
            backup,
            cancel_file,
            open: None,
            last: (String::new(), 0),
        }
    }

    fn backup_dir(&self) -> Result<&Path, String> {
        self.backup.as_deref().ok_or_else(|| "no backup folder was given".to_string())
    }

    /// A stored file's path: images compressed, the small files as they are.
    fn stored(&self, name: &str) -> Result<PathBuf, String> {
        plain(name)?;
        let d = self.backup_dir()?;
        Ok(if name.ends_with(".img") { d.join(format!("{name}.gz")) } else { d.join(name) })
    }
}

impl PcEdge for DirEdge {
    fn save(&mut self, record: &Record) -> Result<(), String> {
        write_whole(&self.record, record.to_json().as_bytes())
    }

    fn keep(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
        plain(name)?;
        std::fs::create_dir_all(&self.kept).map_err(|e| format!("{}: {e}", self.kept.display()))?;
        write_whole(&self.kept.join(name), bytes)
    }

    fn kept(&mut self, name: &str) -> Result<Option<Vec<u8>>, String> {
        plain(name)?;
        read_opt(&self.kept.join(name))
    }

    fn backup_location(&self) -> Option<String> {
        self.backup.as_ref().map(|p| p.display().to_string())
    }

    fn backup_begin(&mut self, name: &str) -> Result<(), String> {
        let p = self.stored(name)?;
        std::fs::create_dir_all(self.backup_dir()?).map_err(|e| e.to_string())?;
        let f = File::create(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        self.open = Some((p, GzEncoder::new(BufWriter::with_capacity(1 << 20, f), Compression::fast())));
        Ok(())
    }

    fn backup_append(&mut self, bytes: &[u8]) -> Result<(), String> {
        let (p, z) = self.open.as_mut().ok_or("no stored file is open")?;
        z.write_all(bytes).map_err(|e| format!("{}: {e}", p.display()))
    }

    fn backup_end(&mut self) -> Result<(), String> {
        let (p, z) = self.open.take().ok_or("no stored file is open")?;
        let w = z.finish().map_err(|e| format!("{}: {e}", p.display()))?;
        let f = w.into_inner().map_err(|e| format!("{}: {e}", p.display()))?;
        f.sync_all().map_err(|e| format!("{}: {e}", p.display()))?;
        if let Some(d) = p.parent() {
            sync_dir(d);
        }
        Ok(())
    }

    fn backup_read(&mut self, name: &str, each: &mut dyn FnMut(&[u8]) -> Result<(), String>) -> Result<(), String> {
        let p = self.stored(name)?;
        let f = File::open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let mut r: Box<dyn Read> = if name.ends_with(".img") { Box::new(GzDecoder::new(f)) } else { Box::new(f) };
        let mut buf = vec![0u8; 4 << 20];
        loop {
            let n = r.read(&mut buf).map_err(|e| format!("{}: {e}", p.display()))?;
            if n == 0 {
                return Ok(());
            }
            each(&buf[..n])?;
        }
    }

    fn backup_put(&mut self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let p = self.stored(name)?;
        std::fs::create_dir_all(self.backup_dir()?).map_err(|e| e.to_string())?;
        write_whole(&p, bytes)
    }

    fn backup_get(&mut self, name: &str) -> Result<Option<Vec<u8>>, String> {
        if self.backup.is_none() {
            return Ok(None);
        }
        read_opt(&self.stored(name)?)
    }

    fn random(&mut self) -> [u8; 16] {
        let mut b = [0u8; 16];
        // The kernel's generator; without it the run stops at its first GUID.
        let mut f = File::open("/dev/urandom").expect("/dev/urandom");
        f.read_exact(&mut b).expect("/dev/urandom");
        b
    }

    fn progress(&mut self, step: &str, done: u64, total: u64) {
        // one line per step and tenth
        let tenth = (done * 10).checked_div(total).unwrap_or(10);
        if self.last.0 != step || self.last.1 != tenth {
            self.last = (step.to_string(), tenth);
            eprintln!("  {step}: {}%", tenth * 10);
        }
    }

    fn note(&mut self, line: &str) {
        println!("{line}");
    }

    fn cancel_requested(&mut self) -> bool {
        self.cancel_file.as_ref().is_some_and(|f| f.exists())
    }
}
