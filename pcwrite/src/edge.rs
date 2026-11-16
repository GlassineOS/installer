// SPDX-License-Identifier: GPL-2.0-or-later
//! The PC executor's edge on Linux: the record in a file (a whole copy under
//! a temporary name, synced, then renamed), what is kept before the first
//! write in `RECORD.kept/`, and the backup in a folder, each disk's image
//! gzip-compressed (`disk<N>.img.gz`), every file synced before it counts.
//! An image larger than a FAT32 file may be (4 GiB) goes on in
//! `disk<N>.img.gz.1`, `.2`, ...: the parts, read in order, are the one
//! compressed stream.

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
    /// The most bytes one stored file holds before the next part begins.
    pub segment: u64,
    open: Option<(PathBuf, GzEncoder<Parts>)>,
    last: (String, u64),
}

/// The default part size: below FAT32's 4 GiB file limit.
pub const SEGMENT: u64 = 2 << 30;

/// A compressed image's parts: `NAME`, `NAME.1`, `NAME.2`, ...
fn part_path(first: &Path, i: usize) -> PathBuf {
    if i == 0 {
        return first.to_path_buf();
    }
    let mut s = first.as_os_str().to_owned();
    s.push(format!(".{i}"));
    PathBuf::from(s)
}

/// Writes across parts of at most `segment` bytes each.
struct Parts {
    first: PathBuf,
    segment: u64,
    files: Vec<File>,
    cur: BufWriter<File>,
    in_cur: u64,
}

impl Parts {
    fn create(first: &Path, segment: u64) -> std::io::Result<Parts> {
        // parts left by an earlier, cut attempt
        let mut i = 1;
        while std::fs::remove_file(part_path(first, i)).is_ok() {
            i += 1;
        }
        Ok(Parts {
            first: first.to_path_buf(),
            segment: segment.max(1),
            files: vec![],
            cur: BufWriter::with_capacity(1 << 20, File::create(first)?),
            in_cur: 0,
        })
    }

    fn finish(mut self) -> std::io::Result<()> {
        self.cur.flush()?;
        let last = self.cur.into_inner().map_err(|e| e.into_error())?;
        self.files.push(last);
        for f in &self.files {
            f.sync_all()?;
        }
        Ok(())
    }
}

impl Write for Parts {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        if self.in_cur == self.segment {
            self.cur.flush()?;
            let next = File::create(part_path(&self.first, self.files.len() + 1))?;
            let done = std::mem::replace(&mut self.cur, BufWriter::with_capacity(1 << 20, next));
            self.files.push(done.into_inner().map_err(|e| e.into_error())?);
            self.in_cur = 0;
        }
        let n = (b.len() as u64).min(self.segment - self.in_cur) as usize;
        let n = self.cur.write(&b[..n])?;
        self.in_cur += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.cur.flush()
    }
}

/// Reads the parts in order as one stream.
struct ReadParts {
    first: PathBuf,
    next: usize,
    cur: File,
}

impl Read for ReadParts {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let n = self.cur.read(b)?;
            if n > 0 || b.is_empty() {
                return Ok(n);
            }
            match File::open(part_path(&self.first, self.next)) {
                Ok(f) => {
                    self.cur = f;
                    self.next += 1;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
                Err(e) => return Err(e),
            }
        }
    }
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
            segment: SEGMENT,
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
        let f = Parts::create(&p, self.segment).map_err(|e| format!("{}: {e}", p.display()))?;
        self.open = Some((p, GzEncoder::new(f, Compression::fast())));
        Ok(())
    }

    fn backup_append(&mut self, bytes: &[u8]) -> Result<(), String> {
        let (p, z) = self.open.as_mut().ok_or("no stored file is open")?;
        z.write_all(bytes).map_err(|e| format!("{}: {e}", p.display()))
    }

    fn backup_end(&mut self) -> Result<(), String> {
        let (p, z) = self.open.take().ok_or("no stored file is open")?;
        let w = z.finish().map_err(|e| format!("{}: {e}", p.display()))?;
        w.finish().map_err(|e| format!("{}: {e}", p.display()))?;
        if let Some(d) = p.parent() {
            sync_dir(d);
        }
        Ok(())
    }

    fn backup_read(&mut self, name: &str, each: &mut dyn FnMut(&[u8]) -> Result<(), String>) -> Result<(), String> {
        let p = self.stored(name)?;
        let f = File::open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        let mut r: Box<dyn Read> = if name.ends_with(".img") {
            Box::new(GzDecoder::new(ReadParts {
                first: p.clone(),
                next: 1,
                cur: f,
            }))
        } else {
            Box::new(f)
        };
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
