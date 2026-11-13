// SPDX-License-Identifier: GPL-2.0-or-later
//! `glassine-install backup --ram IMAGE --out DIR`: the full backup through
//! the RAM environment (spec 001 FR-009), on Linux. The core runs the steps
//! (`ram_backup`); this edge waits for the RAM environment's USB device and
//! keeps each disk as a gzip file in DIR, with the manifest beside them.

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use glassine_installer::glassine_read::Disk;
use glassine_installer::inspect::{Host, UsbIds};
use glassine_installer::ram_backup::{self, BackupEdge, BackupError, BackupOptions, RamUsb};
use glassine_installer_usb as usb;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// The bound on one transfer from the RAM environment: a hash of 64 MiB is
/// read from the disk first.
const RAM_TRANSFER_TIMEOUT: Duration = Duration::from_secs(30);
const PROGRESS_EVERY: u64 = 256 << 20;

pub struct FileEdge {
    dir: PathBuf,
    writing: Option<(String, GzEncoder<BufWriter<File>>)>,
    reading: Option<GzDecoder<BufReader<File>>>,
    shown: u64,
    started: Instant,
}

impl FileEdge {
    /// DIR must be new or empty: a backup never overwrites anything.
    pub fn new(dir: &Path) -> Result<FileEdge, String> {
        if dir.exists() {
            let mut it = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            if it.next().is_some() {
                return Err(format!("{} isn't empty; a backup goes into a new or empty directory", dir.display()));
            }
        } else {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        Ok(FileEdge {
            dir: dir.to_path_buf(),
            writing: None,
            reading: None,
            shown: 0,
            started: Instant::now(),
        })
    }

    fn file(disk: &Disk) -> String {
        format!("{}.img.gz", disk.name)
    }
}

impl BackupEdge for FileEdge {
    type Ram = usb::UsbTransport;

    async fn wait_ram(&mut self, want: &RamUsb, seconds: u64) -> Result<(usb::UsbTransport, UsbIds), String> {
        let deadline = Instant::now() + Duration::from_secs(seconds);
        let mut last = String::from("nothing came up");
        loop {
            match usb::find_kind(usb::Kind::RamRead) {
                Ok(info) if info.vendor_id() == want.vendor_id && info.product_id() == want.product_id => match usb::open_kind(&info, usb::Kind::RamRead) {
                    Ok((mut t, ids)) => {
                        t.set_timeout(RAM_TRANSFER_TIMEOUT);
                        return Ok((t, ids));
                    }
                    Err(e) => last = e.to_string(),
                },
                Ok(info) => last = format!("a reader interface on {:04x}:{:04x}", info.vendor_id(), info.product_id()),
                Err(usb::OpenError::NoDevice) => {}
                Err(e) => last = e.to_string(),
            }
            if Instant::now() >= deadline {
                return Err(format!("no {want} within {seconds} s ({last})"));
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    fn begin(&mut self, disk: &Disk) -> Result<(), String> {
        let name = Self::file(disk);
        let p = self.dir.join(&name);
        let f = File::create_new(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        self.writing = Some((name, GzEncoder::new(BufWriter::with_capacity(1 << 20, f), Compression::fast())));
        self.shown = 0;
        self.started = Instant::now();
        eprintln!("reading {} ({} bytes) into {}", disk.name, disk.size, p.display());
        Ok(())
    }

    fn write(&mut self, data: &[u8]) -> Result<(), String> {
        let (name, w) = self.writing.as_mut().ok_or("no copy begun")?;
        w.write_all(data).map_err(|e| format!("{name}: {e}"))
    }

    fn end(&mut self, _disk: &Disk) -> Result<String, String> {
        let (name, w) = self.writing.take().ok_or("no copy begun")?;
        let buf = w.finish().map_err(|e| format!("{name}: {e}"))?;
        let f = buf.into_inner().map_err(|e| format!("{name}: {e}"))?;
        f.sync_all().map_err(|e| format!("{name}: {e}"))?;
        Ok(name)
    }

    fn reopen(&mut self, disk: &Disk) -> Result<(), String> {
        let p = self.dir.join(Self::file(disk));
        let f = File::open(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        self.reading = Some(GzDecoder::new(BufReader::with_capacity(1 << 20, f)));
        eprintln!("{}: checking the stored copy", disk.name);
        Ok(())
    }

    fn fill(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let r = self.reading.as_mut().ok_or("no copy reopened")?;
        // fill as much as asked, unless the copy ends first
        let mut n = 0;
        while n < buf.len() {
            match r.read(&mut buf[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(n)
    }

    fn manifest(&mut self, text: &str) -> Result<(), String> {
        let p = self.dir.join("manifest.json");
        let mut f = File::create_new(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        f.write_all(text.as_bytes())
            .and_then(|_| f.sync_all())
            .map_err(|e| format!("{}: {e}", p.display()))
    }

    fn progress(&mut self, disk: &Disk, done: u64) {
        if done - self.shown >= PROGRESS_EVERY || done == disk.size {
            self.shown = done;
            let s = self.started.elapsed().as_secs_f64().max(0.001);
            eprintln!(
                "  {}: {} of {} MiB ({:.0}%, {:.1} MB/s)",
                disk.name,
                done >> 20,
                disk.size >> 20,
                done as f64 * 100.0 / disk.size.max(1) as f64,
                done as f64 / s / 1e6
            );
        }
    }

    fn note(&mut self, line: &str) {
        eprintln!("{line}");
    }
}

/// Runs the backup against the one connected fastboot device.
pub fn run(ds: &[&glassine_installer::format1::Description], image: &[u8], out: &Path, accept_slot_flags: bool) -> Result<(), String> {
    if std::env::consts::OS != "linux" {
        return Err("backup runs on Linux only for now".into());
    }
    let mut edge = FileEdge::new(out)?;
    let info = usb::find().map_err(|e| e.to_string())?;
    let (t, ids) = usb::open(&info).map_err(|e| e.to_string())?;
    let host = Host {
        platform: std::env::consts::OS.to_string(),
        connections: vec!["usb-native".into()],
    };
    let opts = BackupOptions { image, accept_slot_flags };
    match pollster::block_on(ram_backup::back_up_device(ds, t, &ids, &host, opts, &mut edge)) {
        Ok(done) => {
            println!("backup of {} disk(s) in {}:", done.disks.len(), out.display());
            for (r, file) in &done.disks {
                println!("  {file}: {} bytes, sha256 {}", r.disk.size, r.sha256_hex());
            }
            println!("  manifest.json");
            if done.rebooted {
                println!("the device is restarting into its previous system");
            } else {
                println!("the RAM environment didn't answer the reboot; it restarts the device by itself within 10 minutes");
            }
            Ok(())
        }
        Err(BackupError::SlotFlags(w)) => Err(format!("{w}.\nNothing was sent but reads. Pass --accept-slot-flags to go on.")),
        Err(e) => Err(e.to_string()),
    }
}
