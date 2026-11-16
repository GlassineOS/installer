// SPDX-License-Identifier: GPL-2.0-or-later
//! The PC writer: the only code of the installer that writes a PC's disks
//! and its firmware's variables. It is the core's [`Machine`] on Linux, and
//! [`DirEdge`] keeps the install record, what is kept before the first
//! write and the backup in folders on another disk.
//!
//! - Every disk is opened read-only, except the plan's target disk, opened
//!   for writing once with `O_EXCL` (the kernel refuses it while anything
//!   holds the disk) and only when nothing on it is mounted or used as swap.
//! - Reads go past the page cache (`O_DIRECT`, aligned), so a read-back
//!   compares what the disk holds, not what was just handed to the kernel.
//! - A flush is `fdatasync`; after a table write the kernel is asked to
//!   reread the table (`BLKRRPART`, best effort).
//! - A firmware variable is written whole in one `write`, its immutable flag
//!   cleared first where efivarfs set it.
//!
//! Everything is below a root directory (`/` on a real machine): tests point
//! it at a tree of plain files (`dev/<name>`, `sys/...`, `proc/...`).
#![cfg(target_os = "linux")]

mod edge;
pub use edge::{DirEdge, SEGMENT};

use glassine_installer::pc_execute::{DiskId, Machine};
use glassine_installer_pc::{DiskName, disk_names};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

const ALIGN: usize = 4096;
const VARS: &str = "sys/firmware/efi/efivars";
/// `_IO(0x12, 95)`
const BLKRRPART: libc::c_ulong = 0x125f;
const FS_IMMUTABLE_FL: libc::c_long = 0x10;
/// `_IOR('f', 1, long)` and `_IOW('f', 2, long)` on 64-bit Linux.
const FS_IOC_GETFLAGS: libc::c_ulong = 0x8008_6601;
const FS_IOC_SETFLAGS: libc::c_ulong = 0x4008_6602;

struct Open {
    id: DiskId,
    /// for writing (the target only) or reading
    file: File,
    /// past the cache, where the device or file system offers it
    direct: Option<File>,
}

/// The machine below `root`.
pub struct LinuxMachine {
    root: PathBuf,
    disks: Vec<(DiskName, Option<Open>)>,
    target: u32,
}

impl LinuxMachine {
    /// The machine, with `target` the only disk that may be written. Refuses
    /// a target something holds (a mounted file system, swap), and one the
    /// kernel won't give exclusively.
    pub fn new(root: &Path, target: u32) -> Result<LinuxMachine, String> {
        let names = disk_names(root)?;
        let t = names.iter().find(|d| d.index == target).ok_or_else(|| format!("disk {target} isn't there"))?;
        if t.in_use {
            return Err(format!(
                "disk {target} ({}) has a file system mounted or swap on it: nothing is written to a disk in use",
                t.name
            ));
        }
        Ok(LinuxMachine {
            root: root.to_path_buf(),
            disks: names.into_iter().map(|n| (n, None)).collect(),
            target,
        })
    }

    fn dev(&self, name: &str) -> PathBuf {
        self.root.join("dev").join(name)
    }

    fn open(&mut self, index: u32) -> Result<&mut Open, String> {
        let pos = self
            .disks
            .iter()
            .position(|(d, _)| d.index == index)
            .ok_or_else(|| format!("disk {index} isn't there"))?;
        if self.disks[pos].1.is_none() {
            let d = &self.disks[pos].0;
            let p = self.dev(&d.name);
            let write = index == self.target;
            let file = OpenOptions::new()
                .read(true)
                .write(write)
                .custom_flags(if write { libc::O_EXCL } else { 0 })
                .open(&p)
                .map_err(|e| format!("disk {index} ({}): {e}", d.name))?;
            let direct = OpenOptions::new().read(true).custom_flags(libc::O_DIRECT).open(&p).ok();
            let size = d.info.size;
            self.disks[pos].1 = Some(Open {
                id: DiskId {
                    index,
                    size,
                    sector: d.info.sector.max(512),
                    kind: d.info.kind.clone(),
                    model: d.info.model.clone(),
                },
                file,
                direct,
            });
        }
        Ok(self.disks[pos].1.as_mut().unwrap())
    }

    fn var_path(&self, name: &str) -> Result<PathBuf, String> {
        if name.is_empty() || name.contains('/') || name.starts_with('.') {
            return Err(format!("{name:?} isn't a variable's name"));
        }
        Ok(self.root.join(VARS).join(name))
    }
}

const EFIVARFS_MAGIC: i64 = 0xde5e_81e4;

fn on_efivarfs(dir: &Path) -> bool {
    let Ok(c) = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    // SAFETY: statfs into a zeroed struct with a NUL-terminated path.
    unsafe {
        let mut s: libc::statfs = std::mem::zeroed();
        libc::statfs(c.as_ptr(), &mut s) == 0 && s.f_type as i64 == EFIVARFS_MAGIC
    }
}

/// An `ALIGN`-aligned buffer of `len` bytes (a whole number of `ALIGN`s).
struct Aligned {
    v: Vec<u8>,
    off: usize,
    len: usize,
}

impl Aligned {
    fn new(len: usize) -> Aligned {
        let v = vec![0u8; len + ALIGN];
        let off = (ALIGN - (v.as_ptr() as usize % ALIGN)) % ALIGN;
        Aligned { v, off, len }
    }
    fn buf(&mut self) -> &mut [u8] {
        &mut self.v[self.off..self.off + self.len]
    }
}

impl Machine for LinuxMachine {
    fn disks(&mut self) -> Result<Vec<DiskId>, String> {
        let idx: Vec<u32> = self.disks.iter().map(|(d, _)| d.index).collect();
        idx.into_iter().map(|i| self.open(i).map(|o| o.id.clone())).collect()
    }

    fn read(&mut self, disk: u32, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let o = self.open(disk)?;
        if offset + buf.len() as u64 > o.id.size {
            return Err(format!("a read past the end of disk {disk}"));
        }
        let start = offset / ALIGN as u64 * ALIGN as u64;
        let end = (offset + buf.len() as u64).div_ceil(ALIGN as u64) * ALIGN as u64;
        // A disk whose size isn't a whole number of ALIGNs: its last bytes
        // are read through the cache, dropped first (below).
        let direct = if end <= o.id.size { o.direct.as_ref() } else { None };
        if let Some(d) = direct {
            let mut a = Aligned::new((end - start) as usize);
            match d.read_exact_at(a.buf(), start) {
                Ok(()) => {
                    let skip = (offset - start) as usize;
                    buf.copy_from_slice(&a.buf()[skip..skip + buf.len()]);
                    return Ok(());
                }
                // a file system without direct reads (tmpfs): drop the cache below
                Err(e) if e.raw_os_error() == Some(libc::EINVAL) => o.direct = None,
                Err(e) => return Err(format!("disk {disk}: {e}")),
            }
        }
        // SAFETY: a plain advisory call on an open descriptor.
        unsafe {
            libc::posix_fadvise(o.file.as_raw_fd(), offset as i64, buf.len() as i64, libc::POSIX_FADV_DONTNEED);
        }
        o.file.read_exact_at(buf, offset).map_err(|e| format!("disk {disk}: {e}"))
    }

    fn write(&mut self, disk: u32, offset: u64, data: &[u8]) -> Result<(), String> {
        if disk != self.target {
            return Err(format!("disk {disk} isn't the disk the plan writes: refused"));
        }
        let o = self.open(disk)?;
        if offset + data.len() as u64 > o.id.size {
            return Err(format!("a write past the end of disk {disk}"));
        }
        o.file.write_all_at(data, offset).map_err(|e| format!("disk {disk}: {e}"))
    }

    fn flush(&mut self, disk: u32) -> Result<(), String> {
        let o = self.open(disk)?;
        o.file.sync_data().map_err(|e| format!("disk {disk}: {e}"))
    }

    fn table_written(&mut self, disk: u32) {
        if let Ok(o) = self.open(disk) {
            // SAFETY: an ioctl without an argument on an open descriptor; it
            // fails harmlessly on a file or a disk whose partitions are busy.
            unsafe {
                libc::ioctl(o.file.as_raw_fd(), BLKRRPART as _);
            }
        }
    }

    fn var_names(&mut self) -> Result<Vec<String>, String> {
        let dir = self.root.join(VARS);
        let mut v: Vec<String> = std::fs::read_dir(&dir)
            .map_err(|e| format!("the firmware's variables: {e}"))?
            .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect();
        v.sort();
        Ok(v)
    }

    fn var(&mut self, name: &str) -> Result<Option<Vec<u8>>, String> {
        let p = self.var_path(name)?;
        let mut f = match File::open(&p) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(format!("{name}: {e}")),
        };
        let mut b = vec![];
        f.read_to_end(&mut b).map_err(|e| format!("{name}: {e}"))?;
        Ok(Some(b))
    }

    fn set_var(&mut self, name: &str, file: &[u8]) -> Result<(), String> {
        let p = self.var_path(name)?;
        if let Ok(f) = File::open(&p) {
            let mut flags: libc::c_long = 0;
            // SAFETY: the flag ioctls on an open file, with a long to read
            // into and to pass; they fail harmlessly where unsupported.
            unsafe {
                if libc::ioctl(f.as_raw_fd(), FS_IOC_GETFLAGS as _, &mut flags) == 0 && flags & FS_IMMUTABLE_FL != 0 {
                    flags &= !FS_IMMUTABLE_FL;
                    libc::ioctl(f.as_raw_fd(), FS_IOC_SETFLAGS as _, &flags);
                }
            }
        }
        // efivarfs replaces a variable on a write (no truncation, which it
        // doesn't offer); a plain file standing for one is truncated.
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(!on_efivarfs(&self.root.join(VARS)))
            .open(&p)
            .map_err(|e| format!("{name}: {e}"))?;
        // efivarfs takes a variable in one write
        let n = f.write(file).map_err(|e| format!("{name}: {e}"))?;
        if n != file.len() {
            return Err(format!("{name}: {n} of {} bytes written", file.len()));
        }
        Ok(())
    }
}
