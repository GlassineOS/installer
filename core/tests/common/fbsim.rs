// SPDX-License-Identifier: GPL-2.0-or-later
//! A simulated fastboot boot loader with partitions behind it, for the write
//! paths (`flash`, `oem`, `reboot-bootloader`) and the steps built on them.
//!
//! Two ways of counting a transfer:
//! - [`Wire::Standard`]: as the protocol specifies, a transfer is what was
//!   sent; a command is at most 64 bytes.
//! - [`Wire::NoteAir`]: as both Note Air boot loaders count (the same model
//!   as `tests/inspect.rs`): the first transfer of a boot is counted right,
//!   the second as 6 bytes, every later one as the length of the one before
//!   it, and a 64-byte command buffer keeps what the last command left in it.
//!   Only equal-length transfers (after the warm-up) come through intact: a
//!   short last transfer of a download is counted long and overruns it.
//!
//! The partition store models what the hardware did (2026-11-15): `flash:`
//! writes the whole download from the partition's start and leaves the bytes
//! after it; `flash:` of any name that starts with `partition` erases EVERY
//! partition first (the boot loaders' `UpdatePartitionTable`), and the
//! simulation records that in [`FbSim::erased_whole_disk`]. A sparse image
//! into a partition larger than max-download-size is answered OKAY before it
//! is written (abl's parallel download-and-flash), recorded in
//! [`FbSim::early_okay`].
//!
//! `tests/inspect.rs` and `tests/ram_backup.rs` keep their own smaller
//! simulations of reads and of `download` + `boot`; this one models them too.

use glassine_installer::fastboot::{FbError, Transport};
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wire {
    Standard,
    NoteAir,
}

/// What the simulated device does wrong, if anything.
#[derive(Default, Clone, Copy, Debug)]
pub struct Faults {
    /// The USB link is cut while the download's data transfer number `n`
    /// (from 0) is sent: it fails, and the device is gone.
    pub cut_at_chunk: Option<usize>,
    /// The device is gone after this many OUT transfers (any kind).
    pub vanish_after: Option<usize>,
    /// `flash:` writes the first half of the download, then answers FAIL.
    pub fail_flash: bool,
    /// `download:` is answered OKAY where DATA is due.
    pub okay_for_data: bool,
    /// OKAY after this many data transfers, before the download is complete;
    /// the device leaves the data phase and reads what follows as commands.
    pub okay_after_chunks: Option<usize>,
    /// `download:` is answered DATA with this size instead of the asked one.
    pub data_size: Option<u32>,
}

pub struct FbSim {
    pub wire: Wire,
    pub product: String,
    /// Other variables `getvar` answers (`partition-size` and
    /// `max-download-size` come from the store and `max_download`).
    pub vars: BTreeMap<String, String>,
    pub parts: BTreeMap<String, Vec<u8>>,
    pub max_download: u64,
    /// Variables answered FAIL whatever they hold.
    pub hidden: Vec<String>,
    /// `oem` verbs it knows (answered OKAY; each restarts the device).
    pub oem_verbs: Vec<String>,
    pub faults: Faults,
    /// Every OUT transfer, as sent.
    pub log: Vec<Vec<u8>>,
    /// Every command as the device read it (data transfers left out).
    pub commands: Vec<String>,
    pub erased_whole_disk: bool,
    pub early_okay: bool,
    /// What it restarted into (`reboot`, `bootloader`, an oem verb).
    pub rebooted: Vec<String>,
    pub gone: bool,
    n: usize,
    prev: usize,
    cmdbuf: [u8; 64],
    expect: u64,
    chunks: usize,
    download: Vec<u8>,
    replies: VecDeque<Vec<u8>>,
}

/// The equal-length framing's transfer length on the Note Air.
pub const L: usize = 65535;

impl FbSim {
    pub fn new(wire: Wire, product: &str, max_download: u64, parts: &[(&str, Vec<u8>)]) -> FbSim {
        FbSim {
            wire,
            product: product.into(),
            vars: BTreeMap::new(),
            parts: parts.iter().map(|(n, d)| (n.to_string(), d.clone())).collect(),
            max_download,
            hidden: vec![],
            oem_verbs: vec![],
            faults: Faults::default(),
            log: vec![],
            commands: vec![],
            erased_whole_disk: false,
            early_okay: false,
            rebooted: vec![],
            gone: false,
            n: 0,
            prev: 0,
            cmdbuf: [0; 64],
            expect: 0,
            chunks: 0,
            download: vec![],
            replies: VecDeque::new(),
        }
    }

    /// The stock Note Air boot loader (product sdm660, max-download-size
    /// 595230720) with these partitions.
    pub fn stock_noteair(parts: &[(&str, Vec<u8>)]) -> FbSim {
        FbSim::new(Wire::NoteAir, "sdm660", 595230720, parts)
    }

    /// The GlassineOS boot loader (product NoteAir), which also knows
    /// `oem reboot-recovery`.
    pub fn glassine_noteair(parts: &[(&str, Vec<u8>)]) -> FbSim {
        let mut s = FbSim::new(Wire::NoteAir, "NoteAir", 595230720, parts);
        s.oem_verbs = vec!["reboot-recovery".into(), "reboot-edl".into()];
        s
    }

    /// A new boot of the same device: the transfer counting starts over.
    pub fn boot_again(&mut self) {
        self.gone = false;
        self.n = 0;
        self.prev = 0;
        self.cmdbuf = [0; 64];
        self.expect = 0;
        self.download.clear();
        self.replies.clear();
    }

    /// True when any OUT transfer holds `text` (padded or not).
    pub fn sent(&self, text: &str) -> bool {
        self.log.iter().any(|t| t.windows(text.len()).any(|w| w == text.as_bytes()))
    }

    /// How many bytes this transfer counts as.
    fn count(&self, d: &[u8]) -> usize {
        match self.wire {
            Wire::Standard => d.len(),
            Wire::NoteAir => match self.n {
                0 => d.len(),
                1 => 6,
                _ => self.prev,
            },
        }
    }

    fn command(&mut self, d: &[u8], count: usize) -> Option<String> {
        match self.wire {
            Wire::Standard if d.len() > 64 => None,
            Wire::Standard => Some(String::from_utf8_lossy(d).into_owned()),
            Wire::NoteAir => {
                let k = d.len().min(64);
                self.cmdbuf[..k].copy_from_slice(&d[..k]);
                let mut cmd = self.cmdbuf[..count.min(63)].to_vec();
                if let Some(z) = cmd.iter().position(|&b| b == 0) {
                    cmd.truncate(z);
                }
                Some(String::from_utf8_lossy(&cmd).into_owned())
            }
        }
    }

    fn reply(&mut self, r: &str) {
        self.replies.push_back(r.as_bytes().to_vec());
    }

    fn data(&mut self, d: &[u8], count: usize) -> Result<(), FbError> {
        if self.faults.cut_at_chunk == Some(self.chunks) {
            self.gone = true;
            return Err(FbError::Usb("the cable was pulled".into()));
        }
        self.chunks += 1;
        // A transfer counted longer than it was brings bytes the host never sent.
        let mut got = d[..count.min(d.len())].to_vec();
        got.resize(count, 0xee);
        if got.len() as u64 > self.expect {
            self.expect = 0;
            self.download.clear();
            self.reply("FAILdata overrun");
            return Ok(());
        }
        self.download.extend_from_slice(&got);
        self.expect -= got.len() as u64;
        if self.expect == 0 {
            self.reply("OKAY");
        } else if self.faults.okay_after_chunks == Some(self.chunks) {
            self.expect = 0;
            self.reply("OKAY");
        }
        Ok(())
    }

    fn flash(&mut self, name: &str) {
        if name.starts_with("partition") {
            // The table update erases the whole eMMC first.
            for p in self.parts.values_mut() {
                p.fill(0);
            }
            self.erased_whole_disk = true;
            return self.reply("OKAY");
        }
        let Some(part) = self.parts.get_mut(name) else {
            return self.reply("FAILPartition not found");
        };
        let img = std::mem::take(&mut self.download);
        if img.is_empty() {
            return self.reply("FAILno data");
        }
        if img.len() > part.len() {
            self.download = img;
            return self.reply("FAILimage too large");
        }
        if self.faults.fail_flash {
            let h = img.len() / 2;
            part[..h].copy_from_slice(&img[..h]);
            self.download = img;
            return self.reply("FAILError flashing partition : Device Error");
        }
        let sparse = img.starts_with(&0xed26_ff3au32.to_le_bytes());
        if sparse && part.len() as u64 > self.max_download {
            self.early_okay = true;
        }
        part[..img.len()].copy_from_slice(&img);
        self.download = img;
        self.reply("OKAY");
    }

    fn run(&mut self, c: &str) {
        self.commands.push(c.to_string());
        if let Some(v) = c.strip_prefix("getvar:") {
            let ans = if self.hidden.iter().any(|h| h == v) {
                None
            } else if v == "product" {
                Some(self.product.clone())
            } else if v == "max-download-size" {
                Some(self.max_download.to_string())
            } else if let Some(p) = v.strip_prefix("partition-size:") {
                self.parts.get(p).map(|d| format!("0x{:x}", d.len()))
            } else {
                self.vars.get(v).cloned()
            };
            match ans {
                Some(a) => self.reply(&format!("OKAY{a}")),
                None => self.reply("FAILunknown variable"),
            }
        } else if let Some(h) = c.strip_prefix("download:") {
            let Ok(n) = u64::from_str_radix(h, 16) else {
                return self.reply("FAILbad size");
            };
            if n == 0 || n > self.max_download {
                return self.reply("FAILdata too large");
            }
            if self.faults.okay_for_data {
                return self.reply("OKAY");
            }
            self.expect = n;
            self.chunks = 0;
            self.download.clear();
            let said = self.faults.data_size.map_or(n, |s| s as u64);
            self.reply(&format!("DATA{said:08x}"));
        } else if let Some(p) = c.strip_prefix("flash:") {
            self.flash(p);
        } else if let Some(v) = c.strip_prefix("oem ") {
            if self.oem_verbs.iter().any(|o| o == v) {
                self.reply("OKAY");
                self.rebooted.push(v.to_string());
                self.gone = true;
            } else {
                self.reply("FAILunknown command");
            }
        } else if c == "reboot" || c == "reboot-bootloader" {
            self.reply("OKAY");
            self.rebooted.push(if c == "reboot" { "reboot".into() } else { "bootloader".into() });
            self.gone = true;
        } else {
            self.reply("FAILunknown command");
        }
    }
}

impl Transport for FbSim {
    async fn send(&mut self, d: &[u8]) -> Result<(), FbError> {
        self.log.push(d.to_vec());
        if self.gone {
            return Err(FbError::Usb("no such device".into()));
        }
        if self.faults.vanish_after.is_some_and(|k| self.log.len() > k) {
            self.gone = true;
            return Err(FbError::Usb("no such device".into()));
        }
        let count = self.count(d);
        let r = if self.expect > 0 {
            self.data(d, count)
        } else {
            match self.command(d, count) {
                None => {
                    self.reply("FAILcommand too long");
                }
                Some(c) => self.run(&c),
            }
            Ok(())
        };
        self.n += 1;
        self.prev = d.len();
        r
    }

    async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
        match self.replies.pop_front() {
            Some(r) => Ok(r),
            None if self.gone => Err(FbError::Usb("no such device".into())),
            // A real transport bounds the wait and cancels; here it ends at once.
            None => Err(FbError::Timeout("no answer".into())),
        }
    }
}
