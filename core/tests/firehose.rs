// SPDX-License-Identifier: GPL-2.0-or-later
//! The emergency read route against a simulated device: a scripted boot ROM
//! (Sahara), then a simulated programmer that answers Firehose requests the
//! way the public sources describe a real one (logs, ACK and NAK, a NAK
//! naming the payload size it takes, rawmode data, a closing answer), over a
//! synthetic eMMC with a GPT. Every transfer the computer sends is recorded:
//! after the programmer starts, each must be an XML document whose one
//! command is configure, getstorageinfo, read or power reset (004 FR-007).
//! No serial, key, programmer or disk of a real unit is in here.

mod common;
use glassine_installer::emergency_read::{self, Done, EmergencyError, Expected, Failure, ImageEdge, Options, PROBE_WAIT, Restart, Scope};
use glassine_installer::fastboot::{FbError, Transport};
use glassine_installer::firehose;
use glassine_installer::format1::Description;
use glassine_installer::glassine_read::Disk;
use glassine_installer::sahara::{self, cmd};
use glassine_installer::supplied::Unsendable;
use glassine_installer::table::crc32;
use pollster::block_on;
use regex_lite::Regex;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, VecDeque};

const SECTOR: usize = 512;
const SECTORS: u64 = 16384; // an 8 MiB eMMC
const ID: u32 = 0x0d;

/// Small reads, so a partition spans several requests and ranges; listening
/// first, as the command line does.
const OPTS: Options = Options {
    chunk: 64 << 10,
    range: 256 << 10,
    listen_first: true,
    resend_configure: true,
};

/// The partitions of the synthetic disk: name, first and last LBA.
const PARTS: &[(&str, u64, u64)] = &[("devinfo", 64, 191), ("boot_a", 256, 2303), ("userdata", 4096, 16350)];

fn programmer() -> Vec<u8> {
    // Stand-in bytes: not a program.
    (0..0x40u32).map(|i| (i * 13) as u8).collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The key hash the simulated boot ROM reports ([`Faults::other_key`]: another).
fn pk_hash(other: bool) -> Vec<u8> {
    (0u8..32).map(|b| b ^ if other { 0x5a } else { 0x3c }).collect()
}

/// The Note Air's key hash and known programmer in its description.
const DESC_PK: &str = "7be49b72f9e4337223ccb84d6eccca4e61ce16e3602ac2008cb18b75babe6d09";
const DESC_KNOWN: &str = "b3e409558677f9b280fed7851e8cc66f0079ad78ef0b07519a3a98d86c14d41b";

/// The Note Air's description for the simulated device: its key hash the
/// simulated boot ROM's, its known programmer the stand-in bytes.
fn desc_text() -> String {
    common::noteair()
        .replace(DESC_PK, &hex(&pk_hash(false)))
        .replace(DESC_KNOWN, &hex(&Sha256::digest(programmer())))
}

fn desc() -> Description {
    common::valid(&desc_text())
}

fn le(v: &mut [u8], at: usize, x: u64, n: usize) {
    v[at..at + n].copy_from_slice(&x.to_le_bytes()[..n]);
}

/// The synthetic eMMC: every sector a pattern of its LBA, a protective MBR,
/// a primary GPT with [`PARTS`].
fn disk() -> Vec<u8> {
    let mut d = vec![0u8; SECTORS as usize * SECTOR];
    for (lba, s) in d.chunks_mut(SECTOR).enumerate() {
        for (i, b) in s.iter_mut().enumerate() {
            *b = (lba as u32).wrapping_mul(31).wrapping_add(i as u32 * 7) as u8;
        }
    }
    d[..SECTOR * 34].fill(0);
    d[446 + 4] = 0xee;
    d[510] = 0x55;
    d[511] = 0xaa;
    let mut entries = vec![0u8; 128 * 128];
    for (i, (name, first, last)) in PARTS.iter().enumerate() {
        let e = &mut entries[i * 128..(i + 1) * 128];
        e[0] = 0xa0 + i as u8; // type
        e[16] = 0x10 + i as u8; // unique
        le(e, 32, *first, 8);
        le(e, 40, *last, 8);
        for (k, c) in name.encode_utf16().enumerate() {
            le(e, 56 + 2 * k, c as u64, 2);
        }
    }
    let mut h = vec![0u8; 92];
    h[..8].copy_from_slice(b"EFI PART");
    le(&mut h, 8, 0x0001_0000, 4);
    le(&mut h, 12, 92, 4);
    le(&mut h, 24, 1, 8);
    le(&mut h, 32, SECTORS - 1, 8);
    le(&mut h, 40, 34, 8);
    le(&mut h, 48, SECTORS - 34, 8);
    h[56] = 0x77;
    le(&mut h, 72, 2, 8);
    le(&mut h, 80, 128, 4);
    le(&mut h, 84, 128, 4);
    le(&mut h, 88, crc32(&entries) as u64, 4);
    let c = crc32(&h);
    le(&mut h, 16, c as u64, 4);
    d[SECTOR..SECTOR + 92].copy_from_slice(&h);
    d[2 * SECTOR..34 * SECTOR].copy_from_slice(&entries);
    // The backup: the entries, then the header at the last LBA.
    let mut b = h.clone();
    le(&mut b, 16, 0, 4);
    le(&mut b, 24, SECTORS - 1, 8);
    le(&mut b, 32, 1, 8);
    le(&mut b, 72, SECTORS - 33, 8);
    let c = crc32(&b);
    le(&mut b, 16, c as u64, 4);
    let end = SECTORS as usize * SECTOR;
    d[end - 33 * SECTOR..end - SECTOR].copy_from_slice(&entries);
    d[end - SECTOR..end].fill(0);
    d[end - SECTOR..end - SECTOR + 92].copy_from_slice(&b);
    d
}

/// A boot area's contents: a pattern of its number and LBA.
fn boot_area(physical: u32, sectors: u64) -> Vec<u8> {
    (0..sectors as usize * SECTOR)
        .map(|i| (i / SECTOR) as u8 ^ (i as u8).wrapping_mul(physical as u8 * 3 + 1))
        .collect()
}

fn pkt(c: u32, body: &[u32]) -> Vec<u8> {
    let mut v = c.to_le_bytes().to_vec();
    v.extend_from_slice(&((8 + 4 * body.len()) as u32).to_le_bytes());
    for w in body {
        v.extend_from_slice(&w.to_le_bytes());
    }
    v
}

/// What the simulated device does wrong, or differently.
#[derive(Clone, Default)]
struct Faults {
    /// The boot ROM reports another chip.
    other_chip: bool,
    /// The boot ROM reports another key hash (the chip is the described one).
    other_key: bool,
    /// Reads past the end of an area get no answer at all (a hang), instead
    /// of a NAK; `power` is still answered.
    silent_past_end: bool,
    /// Reads past the end of an area answer zeros instead of a NAK.
    reads_past_end: bool,
    /// The boot ROM refuses the programmer.
    sahara_refuses: bool,
    /// The programmer never answers.
    silent: bool,
    /// Logs on its own once it starts.
    startup_logs: bool,
    /// The payload size it takes; more is answered NAK naming it.
    payload_max: Option<u64>,
    /// NAK to configure without a size.
    configure_nak: bool,
    /// The auth log before a NAK to configure.
    wants_auth: bool,
    /// Answers getstorageinfo; `block_size` in it.
    storage_info: bool,
    block_size: u64,
    /// The first read that covers this LBA is refused / stops early / reads
    /// one byte differently the second time.
    nak_at: Option<u64>,
    short_at: Option<u64>,
    flip_at: Option<u64>,
    /// Several documents and the data in as few transfers as possible.
    coalesce: bool,
    /// A ZLP after the data.
    zlp: bool,
    /// The most bytes of data per transfer (0: all in one).
    split: usize,
    /// The most bytes of any transfer (0: as asked): documents split too.
    cap: usize,
    /// Drops every request until its start-up logs were read.
    deaf_until_logged: bool,
    /// Answers only from this configure on (1st, 2nd, ...; 0 as 1).
    answer_from: u32,
    /// The eMMC's boot areas (physical partitions 1 and 2), this many
    /// sectors each; none when 0 (reads on them are refused).
    boot_sectors: u64,
    /// getstorageinfo names each physical partition's own size; else it
    /// answers with the user area's numbers whatever it is asked.
    per_partition_info: bool,
    /// getstorageinfo names this physical partition RPMB.
    rpmb_named: Option<u32>,
    /// Ignores the 1st configure and answers it with the 2nd: two answers.
    late_pair: bool,
    /// Like `late_pair`, but the extra answer comes only before the answer to
    /// the next request.
    late_split: bool,
}

struct Device {
    disk: Vec<u8>,
    boot: [Vec<u8>; 2],
    f: Faults,
    out: VecDeque<Vec<u8>>,
    /// Every transfer the computer sent, in order.
    sent: Vec<Vec<u8>>,
    /// The index in `sent` of the Sahara DONE: what follows went to the programmer.
    done_at: Option<usize>,
    reads_covering: BTreeMap<u64, u32>,
    stuck: bool,
    /// Transfers still to be read before it listens ([`Faults::deaf_until_logged`]).
    deaf_pops: usize,
    configures: u32,
    /// A late answer to push before the next answer ([`Faults::late_split`]).
    late: Option<Vec<u8>>,
    /// Every short wait the computer asked for, in order.
    waits: Vec<Option<std::time::Duration>>,
}

fn doc(body: &str) -> Vec<u8> {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<data>\n{body}\n</data>").into_bytes()
}

fn log(v: &str) -> Vec<u8> {
    doc(&format!("<log value=\"{v}\" />"))
}

impl Device {
    fn new(f: Faults) -> Self {
        let hash = pk_hash(f.other_key);
        let msm: u64 = if f.other_chip { 0x0012_3456_0000_0000 } else { 0x000c_c0e1_0000_0000 };
        let mut out: VecDeque<Vec<u8>> = vec![
            pkt(cmd::HELLO, &[2, 1, 0x400, 3, 0, 0, 0, 0, 0, 0]),
            pkt(cmd::CMD_READY, &[]),
            pkt(cmd::EXECUTE_RESP, &[1, 4]),
            0x0102_0304u32.to_le_bytes().to_vec(),
            pkt(cmd::EXECUTE_RESP, &[2, 8]),
            msm.to_le_bytes().to_vec(),
            pkt(cmd::EXECUTE_RESP, &[3, 32]),
            hash,
            pkt(cmd::EXECUTE_RESP, &[7, 4]),
            3u32.to_le_bytes().to_vec(),
            pkt(cmd::HELLO, &[2, 1, 0x400, 0, 0, 0, 0, 0, 0, 0]),
        ]
        .into();
        if !f.other_chip {
            out.push_back(pkt(cmd::READ_DATA, &[ID, 0, 0x40]));
            if f.sahara_refuses {
                out.push_back(pkt(cmd::END_IMAGE_TX, &[ID, 0x25]));
                out.push_back(pkt(cmd::RESET_RESP, &[]));
            } else {
                out.push_back(pkt(cmd::END_IMAGE_TX, &[ID, 0]));
                out.push_back(pkt(cmd::DONE_RESP, &[1]));
            }
        }
        let boot = [boot_area(1, f.boot_sectors), boot_area(2, f.boot_sectors)];
        Device {
            disk: disk(),
            boot,
            f,
            out,
            sent: vec![],
            done_at: None,
            reads_covering: BTreeMap::new(),
            stuck: false,
            deaf_pops: 0,
            configures: 0,
            late: None,
            waits: vec![],
        }
    }

    fn push(&mut self, items: Vec<Vec<u8>>) {
        if self.f.coalesce {
            self.out.push_back(items.concat());
        } else {
            self.out.extend(items);
        }
    }

    fn firehose(&mut self, d: &[u8]) {
        let text = String::from_utf8(d.to_vec()).expect("an XML request");
        let name = Regex::new(r"<data>\s*<([A-Za-z_]+)").unwrap().captures(&text).expect("a command")[1].to_string();
        let attrs: BTreeMap<String, String> = Regex::new(r#"([A-Za-z_]+)="([^"]*)""#)
            .unwrap()
            .captures_iter(&text)
            .map(|c| (c[1].to_string(), c[2].to_string()))
            .collect();
        let num = |k: &str| attrs.get(k).and_then(|v| v.parse::<u64>().ok()).unwrap_or_else(|| panic!("{k} in {text}"));
        if self.f.silent || self.deaf_pops > 0 || self.stuck && name != "power" {
            return;
        }
        if name != "configure"
            && let Some(l) = self.late.take()
        {
            self.out.push_back(l);
        }
        match name.as_str() {
            "configure" => {
                assert_eq!(attrs.get("MemoryName").map(String::as_str), Some("eMMC"));
                self.configures += 1;
                if self.configures < self.f.answer_from || (self.f.late_pair || self.f.late_split) && self.configures == 1 {
                    return;
                }
                let size = num("MaxPayloadSizeToTargetInBytes");
                if self.f.wants_auth {
                    let a = log("ERROR: Only nop and sig tag can be received before authentication.");
                    self.push(vec![a, doc("<response value=\"NAK\" />")]);
                } else if self.f.configure_nak {
                    self.push(vec![log("ERROR: something it doesn't like"), doc("<response value=\"NAK\" />")]);
                } else if let Some(max) = self.f.payload_max.filter(|m| size > *m) {
                    let r = format!(
                        "<response value=\"NAK\" MemoryName=\"eMMC\" MaxPayloadSizeToTargetInBytes=\"{max}\" MaxPayloadSizeToTargetInBytesSupported=\"{max}\" />"
                    );
                    self.push(vec![log("ERROR: MaxPayloadSizeToTargetInBytes too large"), doc(&r)]);
                } else {
                    let r = format!(
                        "<response value=\"ACK\" MinVersionSupported=\"1\" MemoryName=\"eMMC\" MaxPayloadSizeFromTargetInBytes=\"4096\" \
                         MaxPayloadSizeToTargetInBytes=\"{size}\" MaxPayloadSizeToTargetInBytesSupported=\"{size}\" MaxXMLSizeInBytes=\"4096\" Version=\"1\" TargetName=\"8953\" />"
                    );
                    if self.configures == 2 && self.f.late_pair {
                        self.push(vec![doc(&r), doc(&r)]);
                    } else if self.configures == 2 && self.f.late_split {
                        self.push(vec![doc(&r)]);
                        self.late = Some(doc(&r));
                    } else {
                        self.push(vec![doc(&r)]);
                    }
                }
            }
            "getstorageinfo" => {
                let p = num("physical_partition_number");
                assert!(p < 3, "getstorageinfo on physical partition {p}");
                if self.f.storage_info {
                    let total = if p == 0 || !self.f.per_partition_info { SECTORS } else { self.f.boot_sectors };
                    let name = if self.f.rpmb_named == Some(p as u32) {
                        r#", "phys_partition_name":"RPMB""#
                    } else {
                        ""
                    };
                    // Unescaped quotes, and a serial: what programmers send.
                    let j = format!(
                        r#"INFO: {{"storage_info": {{"total_blocks":{total}, "block_size":{}, "page_size":512, "num_physical":4, "serial_num":305419896, "fw_version":"1", "mem_type":"eMMC", "prod_name":"SYNTH"{name}}}}}"#,
                        self.f.block_size
                    );
                    self.push(vec![
                        log("INFO: Calling handler for getstorageinfo"),
                        log(&j),
                        doc("<response value=\"ACK\" />"),
                    ]);
                } else {
                    self.push(vec![log("ERROR: Unrecognized tag"), doc("<response value=\"NAK\" />")]);
                }
            }
            "read" => {
                assert_eq!(num("SECTOR_SIZE_IN_BYTES"), SECTOR as u64);
                let p = num("physical_partition_number");
                assert!(p < 3, "a read on physical partition {p}: {text}");
                let (start, n) = (num("start_sector"), num("num_partition_sectors"));
                let area = if p == 0 { &self.disk } else { &self.boot[p as usize - 1] };
                assert!(n > 0);
                if start + n > (area.len() / SECTOR) as u64 {
                    if self.f.silent_past_end {
                        // Hangs: nothing comes, not even a log.
                        return;
                    }
                    if self.f.reads_past_end {
                        let mut items = vec![doc("<response value=\"ACK\" rawmode=\"true\" />")];
                        items.push(vec![0u8; n as usize * SECTOR]);
                        items.push(doc("<response value=\"ACK\" rawmode=\"false\" />"));
                        self.push(items);
                        return;
                    }
                    // Past the end, or an area the device doesn't have.
                    self.push(vec![log("ERROR: sectors out of range"), doc("<response value=\"NAK\" />")]);
                    return;
                }
                let covers = |at: Option<u64>| p == 0 && at.is_some_and(|a| a >= start && a < start + n);
                if covers(self.f.nak_at) {
                    self.f.nak_at = None;
                    self.push(vec![log("ERROR: Failed to read sectors"), doc("<response value=\"NAK\" />")]);
                    return;
                }
                let mut data = area[start as usize * SECTOR..(start + n) as usize * SECTOR].to_vec();
                let times = self.reads_covering.entry(start + (p << 48)).or_default();
                *times += 1;
                if let Some(a) = self.f.flip_at.filter(|a| p == 0 && *a >= start && *a < start + n)
                    && *times == 2
                {
                    data[(a - start) as usize * SECTOR] ^= 1;
                }
                let mut items = vec![log("INFO: reading"), doc("<response value=\"ACK\" rawmode=\"true\" />")];
                if covers(self.f.short_at) {
                    data.truncate(data.len() / 2);
                    self.stuck = true;
                }
                let split = if self.f.split == 0 { data.len() } else { self.f.split };
                for c in data.chunks(split) {
                    items.push(c.to_vec());
                }
                if self.stuck {
                    self.push(items);
                    return;
                }
                if self.f.zlp {
                    // A ZLP never coalesces with anything: its own transfer.
                    self.push(items);
                    self.out.push_back(vec![]);
                    items = vec![];
                }
                items.push(doc("<response value=\"ACK\" rawmode=\"false\" />"));
                self.push(items);
            }
            "power" => {
                assert_eq!(attrs.get("value").map(String::as_str), Some("reset"));
                self.out.push_back(doc("<response value=\"ACK\" />"));
            }
            other => panic!("the programmer was sent {other}"),
        }
    }
}

impl Transport for Device {
    async fn send(&mut self, d: &[u8]) -> Result<(), FbError> {
        self.sent.push(d.to_vec());
        if self.done_at.is_some() {
            self.firehose(d);
        } else if d == sahara::done().as_slice() {
            self.done_at = Some(self.sent.len() - 1);
            if self.f.startup_logs {
                self.out.push_back(log("INFO: Binary build date: synthetic"));
                self.out.push_back(log("INFO: Chip serial num: 305419896 (0x12345678)"));
            }
            if self.f.deaf_until_logged {
                // DONE_RESP and the logs.
                self.deaf_pops = self.out.len();
            }
        }
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
        self.recv_max(4096).await
    }

    fn set_short_wait(&mut self, wait: Option<std::time::Duration>) {
        self.waits.push(wait);
    }

    async fn recv_max(&mut self, max: usize) -> Result<Vec<u8>, FbError> {
        let mut t = self.out.pop_front().ok_or(FbError::Timeout("waiting for an answer".into()))?;
        let max = if self.f.cap > 0 { max.min(self.f.cap) } else { max };
        if t.len() > max {
            let rest = t.split_off(max);
            self.out.push_front(rest);
        } else if self.done_at.is_some() {
            self.deaf_pops = self.deaf_pops.saturating_sub(1);
        }
        Ok(t)
    }
}

/// Stores the images in memory, as the command line's files do: under
/// `NAME.img.partial` until kept.
#[derive(Default)]
struct Mem {
    files: BTreeMap<String, Vec<u8>>,
    writing: Option<String>,
    reading: Option<(Vec<u8>, usize)>,
    manifest: Option<String>,
    notes: Vec<String>,
    /// The stored copy reads back one byte different.
    corrupt_copy: bool,
    /// Every call, in order: "begin NAME", "end NAME", "fill", "keep NAME", ...
    events: Vec<String>,
    /// What a process killed at each write or read-back would leave: the
    /// names stored, and whether the manifest is.
    kills: Vec<(Vec<String>, bool)>,
}

impl Mem {
    fn snapshot(&mut self) {
        self.kills.push((self.files.keys().cloned().collect(), self.manifest.is_some()));
    }
}

impl ImageEdge for Mem {
    fn begin(&mut self, d: &Disk) -> Result<(), String> {
        let n = format!("{}.img.partial", d.name);
        assert!(!self.files.contains_key(&n) && !self.files.contains_key(&format!("{}.img", d.name)));
        self.files.insert(n.clone(), vec![]);
        self.writing = Some(n);
        self.events.push(format!("begin {}", d.name));
        Ok(())
    }
    fn write(&mut self, data: &[u8]) -> Result<(), String> {
        let n = self.writing.clone().ok_or("nothing begun")?;
        self.files.get_mut(&n).unwrap().extend_from_slice(data);
        if self.events.last().map(String::as_str) != Some("write") {
            self.events.push("write".into());
        }
        self.snapshot();
        Ok(())
    }
    fn end(&mut self, d: &Disk) -> Result<(), String> {
        self.writing.take().ok_or("nothing begun")?;
        self.events.push(format!("end {}", d.name));
        Ok(())
    }
    fn keep(&mut self, d: &Disk) -> Result<String, String> {
        let b = self.files.remove(&format!("{}.img.partial", d.name)).ok_or("no copy")?;
        let n = format!("{}.img", d.name);
        self.files.insert(n.clone(), b);
        self.events.push(format!("keep {}", d.name));
        Ok(n)
    }
    fn reopen(&mut self, d: &Disk) -> Result<(), String> {
        self.events.push(format!("reopen {}", d.name));
        let mut b = self.files[&format!("{}.img.partial", d.name)].clone();
        if self.corrupt_copy {
            b[100] ^= 0x40;
        }
        self.reading = Some((b, 0));
        Ok(())
    }
    fn fill(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        let (b, at) = self.reading.as_mut().ok_or("nothing reopened")?;
        let n = buf.len().min(b.len() - *at);
        buf[..n].copy_from_slice(&b[*at..*at + n]);
        *at += n;
        if self.events.last().map(String::as_str) != Some("fill") {
            self.events.push("fill".into());
        }
        self.snapshot();
        Ok(n)
    }
    fn incomplete(&mut self, d: &Disk) -> Result<String, String> {
        self.writing = None;
        let n = format!("{}.img.partial", d.name);
        let b = self.files.remove(&n).ok_or("no such copy")?;
        let m = format!("{}.img.incomplete", d.name);
        self.files.insert(m.clone(), b);
        self.events.push(format!("incomplete {}", d.name));
        Ok(m)
    }
    fn manifest(&mut self, text: &str) -> Result<(), String> {
        assert!(self.manifest.is_none());
        self.manifest = Some(text.to_string());
        self.events.push("manifest".into());
        Ok(())
    }
    fn progress(&mut self, _: &Disk, _: u64) {}
    fn note(&mut self, line: &str) {
        self.notes.push(line.to_string());
    }
}

struct Run {
    r: Result<Done, Failure>,
    dev: Device,
    mem: Mem,
}

fn go_with(f: Faults, scope: Scope, mem: Mem) -> Run {
    go_opts(f, scope, mem, OPTS)
}

fn go_opts(f: Faults, scope: Scope, mem: Mem, opts: Options) -> Run {
    go_dev(Device::new(f), scope, mem, opts)
}

fn go_dev(dev: Device, scope: Scope, mem: Mem, opts: Options) -> Run {
    go_desc(dev, &desc(), scope, mem, opts)
}

/// The checked run ([`emergency_read::run_checked`]) for the device `d`
/// describes.
fn go_desc(dev: Device, d: &Description, scope: Scope, mem: Mem, opts: Options) -> Run {
    let mut dev = dev;
    let mut mem = mem;
    let expected = Expected {
        description: d,
        file: "emergency-programmer",
    };
    let r = block_on(emergency_read::run_checked(&mut dev, &programmer(), expected, &scope, opts, &mut mem));
    let run = Run { r, dev, mem };
    whitelist(&run.dev);
    run
}

fn go(f: Faults, scope: Scope) -> Run {
    go_with(f, scope, Mem::default())
}

/// The commands sent to the programmer, in order (`power` with its value).
fn commands(dev: &Device) -> Vec<String> {
    let Some(at) = dev.done_at else { return vec![] };
    let re = Regex::new(r"^<\?xml[^>]*\?><data><([A-Za-z_]+)\s[^<>]*/></data>$").unwrap();
    dev.sent[at + 1..]
        .iter()
        .map(|d| {
            let t = std::str::from_utf8(d).expect("only XML goes to the programmer");
            let c = re.captures(t).unwrap_or_else(|| panic!("not one command in one document: {t}"));
            c[1].to_string()
        })
        .collect()
}

/// FR-007: everything after the Sahara DONE is one XML document with one of
/// the four commands, `power` only with `reset`. Before DONE only Sahara's
/// own packets and the programmer's bytes went out.
fn whitelist(dev: &Device) {
    for (c, d) in commands(dev).iter().zip(&dev.sent[dev.done_at.map_or(0, |a| a + 1)..]) {
        assert!(["configure", "getstorageinfo", "read", "power"].contains(&c.as_str()), "{c}");
        if c == "power" {
            assert!(std::str::from_utf8(d).unwrap().contains("<power value=\"reset\" />"));
        }
    }
    let upto = dev.done_at.map_or(dev.sent.len(), |a| a + 1);
    let prog = programmer();
    for d in &dev.sent[..upto] {
        if d.as_slice() == prog.as_slice() {
            continue;
        }
        let c = u32::from_le_bytes(d[..4].try_into().unwrap());
        assert!(
            [cmd::HELLO_RESP, cmd::EXECUTE, cmd::EXECUTE_DATA, cmd::SWITCH_MODE, cmd::DONE, cmd::RESET].contains(&c),
            "Sahara packet {c:#x}"
        );
    }
}

fn reads(dev: &Device) -> Vec<(u64, u64)> {
    reads_on(dev, 0)
}

/// The reads (start, sectors) on one physical partition, in order.
fn reads_on(dev: &Device, physical: u32) -> Vec<(u64, u64)> {
    let Some(at) = dev.done_at else { return vec![] };
    let re = Regex::new(&format!(
        r#"num_partition_sectors="(\d+)" physical_partition_number="{physical}" start_sector="(\d+)""#
    ))
    .unwrap();
    dev.sent[at + 1..]
        .iter()
        .filter_map(|d| {
            let t = std::str::from_utf8(d).unwrap();
            re.captures(t).map(|c| (c[2].parse().unwrap(), c[1].parse().unwrap()))
        })
        .collect()
}

fn slice(first: u64, last: u64) -> Vec<u8> {
    disk()[first as usize * SECTOR..(last + 1) as usize * SECTOR].to_vec()
}

fn lively() -> Faults {
    Faults {
        startup_logs: true,
        payload_max: Some(65536),
        storage_info: true,
        block_size: 512,
        coalesce: true,
        zlp: true,
        split: 3000,
        ..Default::default()
    }
}

#[test]
fn a_full_session_reads_only() {
    let run = go(lively(), Scope::Partitions(vec!["devinfo".into(), "boot_a".into()]));
    let done = run.r.unwrap();
    assert_eq!(done.restart, Restart::Done);
    assert_eq!(done.sector, 512);
    assert_eq!(done.disk_size, SECTORS * 512);
    assert!(done.configured.renegotiated);
    assert_eq!(done.configured.payload_to_target, 65536);
    assert_eq!(done.table.entries.len(), 3);
    assert_eq!(run.mem.files["devinfo.img"], slice(64, 191));
    assert_eq!(run.mem.files["boot_a.img"], slice(256, 2303));
    assert_eq!(run.mem.files.len(), 2);
    // The order of the session, and nothing else.
    let c = commands(&run.dev);
    assert_eq!(&c[..3], ["configure", "configure", "getstorageinfo"]);
    assert_eq!(c.last().unwrap(), "power");
    assert!(c[3..c.len() - 1].iter().all(|x| x == "read"));
    // The GPT twice, then every range of each partition twice, in chunks.
    let r = reads(&run.dev);
    assert_eq!(&r[..2], [(0, 34), (0, 34)]);
    // devinfo: 128 sectors = one range, one chunk, read twice.
    assert_eq!(&r[2..4], [(64, 128), (64, 128)]);
    // boot_a: 1 MiB = 4 ranges of 4 chunks, each range twice.
    assert_eq!(r.len(), 4 + 4 * 4 * 2);
    let first = [(256, 128), (384, 128), (512, 128), (640, 128)];
    assert_eq!(&r[4..8], first);
    assert_eq!(&r[8..12], first);
    assert_eq!(r[12], (768, 128));
    let m: serde_json::Value = serde_json::from_str(done.manifest.as_deref().unwrap()).unwrap();
    assert_eq!(m["kind"], "glassine-emergency-backup");
    assert_eq!(m["chip"], "SDM636");
    assert_eq!(m["disks"][1]["name"], "boot_a");
    assert_eq!(m["disks"][1]["start_sector"], 256);
    assert_eq!(m["disks"][1]["size"], 1 << 20);
    assert_eq!(m["incomplete"], serde_json::json!([]));
    let text = run.mem.notes.join("\n") + done.manifest.as_deref().unwrap();
    // Nothing unit-unique is kept: the serial the programmer logs, in any form.
    for s in ["305419896", "12345678", "01020304", "serial"] {
        assert!(!text.contains(s), "{s} in {text}");
    }
    assert!(text.contains("devinfo"));
}

#[test]
fn the_whole_disk_without_storage_info() {
    let f = Faults {
        storage_info: false,
        split: 0,
        ..Default::default()
    };
    let run = go(f, Scope::WholeDisk);
    let done = run.r.unwrap();
    assert!(done.storage.is_none());
    // The size from the GPT's backup header LBA.
    assert_eq!(done.disk_size, SECTORS * 512);
    assert_eq!(run.mem.files["emmc-user.img"], disk());
    assert!(!done.configured.renegotiated);
    assert_eq!(done.restart, Restart::Done);
    // No boot areas on this device: noted, and the run goes on.
    assert_eq!(run.mem.files.len(), 1);
    let m: serde_json::Value = serde_json::from_str(done.manifest.as_deref().unwrap()).unwrap();
    assert_eq!(m["not_read"][0]["name"], "emmc-boot1");
    assert_eq!(m["not_read"][1]["physical_partition"], 2);
    assert!(run.mem.notes.iter().any(|n| n.starts_with("emmc-boot2 (physical partition 2) isn't read")));
    assert_eq!(reads_on(&run.dev, 1), [(0, 1)]);
}

/// The whole disk (001 FR-009): the user area, then both boot areas, each
/// read twice and checked; RPMB never.
#[test]
fn the_whole_disk_with_its_boot_areas() {
    for per_partition_info in [true, false] {
        let f = Faults {
            boot_sectors: 640,
            per_partition_info,
            ..lively()
        };
        let run = go(f, Scope::WholeDisk);
        let done = run.r.unwrap();
        assert_eq!(run.mem.files["emmc-user.img"], disk());
        assert_eq!(run.mem.files["emmc-boot1.img"], boot_area(1, 640));
        assert_eq!(run.mem.files["emmc-boot2.img"], boot_area(2, 640));
        assert_eq!(run.mem.files.len(), 3);
        let m: serde_json::Value = serde_json::from_str(done.manifest.as_deref().unwrap()).unwrap();
        assert_eq!(m["disks"][1]["name"], "emmc-boot1");
        assert_eq!(m["disks"][1]["physical_partition"], 1);
        assert_eq!(m["disks"][2]["physical_partition"], 2);
        assert_eq!(m["disks"][2]["size"], 640 * 512);
        assert_eq!(m["not_read"], serde_json::json!([]));
        // Each range of a boot area twice (320 KiB: two ranges of 4 + 1 chunks).
        let r = reads_on(&run.dev, 1);
        let image: Vec<_> = r.iter().filter(|(_, n)| *n > 1).collect();
        assert_eq!(image.len(), 2 * (4 + 1), "{r:?}");
        if per_partition_info {
            // The size reported and its last sector read: nothing past the end.
            assert_eq!(&r[..2], [(0, 1), (639, 1)]);
            assert!(r.iter().all(|(s, n)| s + n <= 640), "{r:?}");
        }
        // The user area first: no boot area read before its last check.
        let c = commands(&run.dev);
        assert!(c.iter().all(|x| ["configure", "getstorageinfo", "read", "power"].contains(&x.as_str())));
        let kept_user = run.mem.events.iter().position(|e| e == "keep emmc-user").unwrap();
        let begun_boot = run.mem.events.iter().position(|e| e == "begin emmc-boot1").unwrap();
        assert!(kept_user < begun_boot);
    }
}

#[test]
fn a_partition_the_programmer_names_rpmb_is_never_read() {
    let f = Faults {
        boot_sectors: 640,
        per_partition_info: true,
        rpmb_named: Some(2),
        ..lively()
    };
    let run = go(f, Scope::WholeDisk);
    let done = run.r.unwrap();
    assert!(run.mem.files.contains_key("emmc-boot1.img"));
    assert!(!run.mem.files.keys().any(|k| k.starts_with("emmc-boot2")));
    assert!(reads_on(&run.dev, 2).is_empty());
    let m: serde_json::Value = serde_json::from_str(done.manifest.as_deref().unwrap()).unwrap();
    assert_eq!(m["not_read"][0]["name"], "emmc-boot2");
    assert!(m["not_read"][0]["why"].as_str().unwrap().contains("RPMB"));
    // Nothing named physical partition 3 or above was sent.
    let at = run.dev.done_at.unwrap();
    for d in &run.dev.sent[at + 1..] {
        let t = std::str::from_utf8(d).unwrap();
        assert!(!Regex::new(r#"physical_partition_number="([3-9]|\d\d)"#).unwrap().is_match(t), "{t}");
    }
}

#[test]
fn boot_area_names_are_reserved() {
    let run = go(lively(), Scope::Partitions(vec!["emmc-boot1".into()]));
    // Not in the GPT anyway; the name check is what a GPT naming it meets.
    assert!(matches!(run.r.unwrap_err().error, EmergencyError::Partition(_)));
}

/// `--only-boot-areas`: the boot areas the way a whole-disk run reads them
/// (each twice, checked, a temporary name until then, the manifest last),
/// without the user area: the GPT is its only read on physical partition 0.
#[test]
fn only_the_boot_areas() {
    for per_partition_info in [true, false] {
        let f = Faults {
            boot_sectors: 640,
            per_partition_info,
            ..lively()
        };
        let run = go(f, Scope::BootAreas);
        let done = run.r.unwrap();
        assert_eq!(done.restart, Restart::Done);
        assert_eq!(run.mem.files.keys().collect::<Vec<_>>(), ["emmc-boot1.img", "emmc-boot2.img"]);
        assert_eq!(run.mem.files["emmc-boot1.img"], boot_area(1, 640));
        assert_eq!(run.mem.files["emmc-boot2.img"], boot_area(2, 640));
        assert_eq!(reads(&run.dev), [(0, 34), (0, 34)], "the user area was read");
        assert!(!run.mem.events.iter().any(|e| e.contains("emmc-user")));
        for physical in [1, 2] {
            let r = reads_on(&run.dev, physical);
            assert_eq!(r.iter().filter(|(_, n)| *n > 1).count(), 2 * (4 + 1), "{r:?}");
        }
        let m: serde_json::Value = serde_json::from_str(done.manifest.as_deref().unwrap()).unwrap();
        assert_eq!(m["disks"].as_array().unwrap().len(), 2);
        assert_eq!(m["disks"][0]["name"], "emmc-boot1");
        assert_eq!(m["disks"][0]["physical_partition"], 1);
        assert_eq!(m["disks"][1]["physical_partition"], 2);
        assert_eq!(m["disks"][1]["size"], 640 * 512);
        assert_eq!(m["not_read"], serde_json::json!([]));
        // Killed at any write or read-back: never a final name before its
        // check, never a manifest before the end.
        assert_eq!(run.mem.events.last().unwrap(), "manifest");
        assert!(!run.mem.kills.is_empty());
        for (names, manifest) in &run.mem.kills {
            assert!(!manifest, "a manifest before the end");
            assert_eq!(names.iter().filter(|n| n.ends_with(".partial")).count(), 1, "{names:?}");
        }
        assert!(run.mem.kills.iter().any(|(n, _)| n == &["emmc-boot1.img", "emmc-boot2.img.partial"]));
        assert!(!run.mem.kills.iter().any(|(n, _)| n.contains(&"emmc-boot2.img".to_string())));
    }
}

/// RPMB stays refused with `--only-boot-areas`: a boot area the programmer
/// names RPMB is skipped, and nothing for physical partition 3 or above is
/// sent.
#[test]
fn only_the_boot_areas_never_reads_rpmb() {
    let f = Faults {
        boot_sectors: 640,
        per_partition_info: true,
        rpmb_named: Some(2),
        ..lively()
    };
    let run = go(f, Scope::BootAreas);
    let done = run.r.unwrap();
    assert_eq!(run.mem.files.keys().collect::<Vec<_>>(), ["emmc-boot1.img"]);
    assert!(reads_on(&run.dev, 2).is_empty());
    let m: serde_json::Value = serde_json::from_str(done.manifest.as_deref().unwrap()).unwrap();
    assert_eq!(m["not_read"][0]["name"], "emmc-boot2");
    assert!(m["not_read"][0]["why"].as_str().unwrap().contains("RPMB"));
    let at = run.dev.done_at.unwrap();
    for d in &run.dev.sent[at + 1..] {
        let t = std::str::from_utf8(d).unwrap();
        assert!(!Regex::new(r#"physical_partition_number="([3-9]|\d\d)"#).unwrap().is_match(t), "{t}");
    }
}

/// `--only-boot-areas` on a device without boot areas read nothing it was
/// asked for: the run fails with why, and the device is still restarted.
#[test]
fn only_the_boot_areas_without_any_fails_with_why() {
    let run = go(lively(), Scope::BootAreas);
    let f = run.r.unwrap_err();
    assert_eq!(f.restart, Restart::Done);
    assert!(f.manifest.is_none());
    let e = f.error.to_string();
    assert!(
        e.contains("no eMMC boot area was read") && e.contains("emmc-boot1:") && e.contains("emmc-boot2:"),
        "{e}"
    );
    assert!(run.mem.files.is_empty());
    assert_eq!(commands(&run.dev).last().unwrap(), "power");
}

/// 004 Scenario 5: a copy is under its final name only after both reads
/// matched and it was read back; the manifest comes last. Whatever moment
/// the process is killed at, no final-named image is unchecked and no
/// manifest is there before the end.
#[test]
fn an_interrupted_run_leaves_no_final_name() {
    let run = go(lively(), Scope::Partitions(vec!["devinfo".into(), "boot_a".into()]));
    let done = run.r.unwrap();
    assert_eq!(run.mem.events.last().unwrap(), "manifest");
    assert_eq!(
        run.mem.events,
        [
            "begin devinfo",
            "write",
            "end devinfo",
            "reopen devinfo",
            "fill",
            "keep devinfo",
            "begin boot_a",
            "write",
            "end boot_a",
            "reopen boot_a",
            "fill",
            "keep boot_a",
            "manifest"
        ]
    );
    assert!(!run.mem.kills.is_empty());
    for (names, manifest) in &run.mem.kills {
        assert!(!manifest, "a manifest before the end");
        // At most one copy in the works, under its temporary name.
        assert_eq!(names.iter().filter(|n| n.ends_with(".partial")).count(), 1, "{names:?}");
        assert!(!names.contains(&"boot_a.img".to_string()) || !names.contains(&"boot_a.img.partial".to_string()));
    }
    // Killed while boot_a was read: devinfo is checked and kept, boot_a only partial.
    assert!(run.mem.kills.iter().any(|(n, _)| n == &["boot_a.img.partial", "devinfo.img"]));
    assert!(!run.mem.kills.iter().any(|(n, _)| n.contains(&"boot_a.img".to_string())));
    assert_eq!(run.mem.files.keys().collect::<Vec<_>>(), ["boot_a.img", "devinfo.img"]);
    let m: serde_json::Value = serde_json::from_str(done.manifest.as_deref().unwrap()).unwrap();
    assert_eq!(m["disks"][1]["file"], "boot_a.img");
}

#[test]
fn gpt_only_stores_nothing() {
    let run = go(lively(), Scope::GptOnly);
    let done = run.r.unwrap();
    assert!(run.mem.files.is_empty() && done.manifest.is_none() && run.mem.manifest.is_none());
    assert_eq!(reads(&run.dev), [(0, 34), (0, 34)]);
    let t = emergency_read::render_table(&done.table, 512);
    assert!(t.contains("devinfo") && t.contains("start         64") && t.contains("128 sectors"), "{t}");
    assert_eq!(done.restart, Restart::Done);
}

#[test]
fn a_programmer_that_listens_only_after_its_logs() {
    let f = Faults {
        deaf_until_logged: true,
        ..lively()
    };
    let run = go(f.clone(), Scope::GptOnly);
    let done = run.r.unwrap();
    assert_eq!(done.restart, Restart::Done);
    // Its start-up lines are kept, the serial left out.
    assert_eq!(done.configured.logs[0], "INFO: Binary build date: synthetic");
    assert!(done.configured.logs.iter().all(|l| !l.contains("serial")));
    assert!(run.mem.notes.iter().any(|n| n == "the programmer started and logged 2 line(s)"));
    // Without listening first, resending configure gets it too.
    let run = go_opts(f.clone(), Scope::GptOnly, Mem::default(), Options { listen_first: false, ..OPTS });
    run.r.unwrap();
    assert!(commands(&run.dev).iter().filter(|c| *c == "configure").count() >= 2);
    // With neither, configure goes unanswered: what a browser, which can't
    // wait out a timeout, would meet with such a programmer.
    let no = Options {
        listen_first: false,
        resend_configure: false,
        ..OPTS
    };
    let run = go_opts(f, Scope::GptOnly, Mem::default(), no);
    let e = run.r.unwrap_err();
    assert!(matches!(e.error, EmergencyError::NoAnswer(_)), "{:?}", e.error);
    assert_eq!(commands(&run.dev), ["configure"]);
}

#[test]
fn small_transfers_split_documents_and_data() {
    for coalesce in [false, true] {
        let f = Faults {
            cap: 700,
            coalesce,
            ..lively()
        };
        let run = go(f, Scope::Partitions(vec!["devinfo".into()]));
        run.r.unwrap();
        assert_eq!(run.mem.files["devinfo.img"], slice(64, 191));
    }
}

#[test]
fn a_silent_programmer_is_not_working() {
    let run = go(
        Faults {
            silent: true,
            ..Default::default()
        },
        Scope::GptOnly,
    );
    let f = run.r.unwrap_err();
    assert!(matches!(f.error, EmergencyError::NoAnswer(_)), "{:?}", f.error);
    assert!(f.error.to_string().contains("isn't working on this device"));
    assert_eq!(f.restart, Restart::NotPossible);
    // configure every short wait for about 5 s, nothing read, nothing more.
    assert_eq!(commands(&run.dev), vec!["configure"; firehose::RESEND_TRIES as usize]);
    assert_eq!(run.dev.waits, [Some(firehose::RESEND_EVERY), None]);
    // Without resending: one configure.
    let silent = Faults {
        silent: true,
        ..Default::default()
    };
    let run = go_opts(
        silent,
        Scope::GptOnly,
        Mem::default(),
        Options {
            resend_configure: false,
            ..OPTS
        },
    );
    assert!(matches!(run.r.unwrap_err().error, EmergencyError::NoAnswer(_)));
    assert_eq!(commands(&run.dev), ["configure"]);
    assert!(run.dev.waits.is_empty());
}

#[test]
fn a_programmer_that_answers_only_the_third_configure() {
    // lively(): its answer is the payload-size NAK, then the reconfigure.
    let run = go(Faults { answer_from: 3, ..lively() }, Scope::Partitions(vec!["devinfo".into()]));
    let done = run.r.unwrap();
    assert!(done.configured.renegotiated);
    assert_eq!(done.configured.payload_to_target, 65536);
    assert_eq!(run.mem.files["devinfo.img"], slice(64, 191));
    let c = commands(&run.dev);
    assert_eq!(&c[..5], ["configure", "configure", "configure", "configure", "getstorageinfo"]);
    // The short waits end before the reads.
    assert_eq!(run.dev.waits, [Some(firehose::RESEND_EVERY), None]);
    // Without the size NAK.
    let run = go(
        Faults {
            answer_from: 3,
            ..Default::default()
        },
        Scope::Partitions(vec!["devinfo".into()]),
    );
    assert!(!run.r.unwrap().configured.renegotiated);
    assert_eq!(commands(&run.dev)[..4], ["configure", "configure", "configure", "getstorageinfo"]);
}

#[test]
fn a_programmer_that_answers_twice() {
    let pair = Faults {
        late_pair: true,
        ..Default::default()
    };
    let split = Faults {
        late_split: true,
        ..Default::default()
    };
    let pair_nak = Faults { late_pair: true, ..lively() };
    for (f, what) in [
        (pair, "both ACKs together"),
        (split, "the second ACK before the next answer"),
        (pair_nak, "both size NAKs together"),
    ] {
        let run = go(f, Scope::Partitions(vec!["devinfo".into(), "boot_a".into()]));
        let done = run.r.unwrap_or_else(|e| panic!("{what}: {e:?}"));
        assert_eq!(run.mem.files["devinfo.img"], slice(64, 191), "{what}");
        assert_eq!(run.mem.files["boot_a.img"], slice(256, 2303), "{what}");
        assert_eq!(done.restart, Restart::Done, "{what}");
    }
}

#[test]
fn configure_refused_or_auth() {
    for (f, why) in [
        (
            Faults {
                configure_nak: true,
                ..Default::default()
            },
            "refused configure",
        ),
        (
            Faults {
                wants_auth: true,
                ..Default::default()
            },
            "authentication",
        ),
    ] {
        let run = go(f, Scope::WholeDisk);
        let e = run.r.unwrap_err();
        assert!(e.error.to_string().contains(why), "{}", e.error);
        // It answers, so it is asked to restart; nothing is read.
        assert_eq!(commands(&run.dev), ["configure", "power"]);
        assert_eq!(e.restart, Restart::Done);
    }
}

#[test]
fn a_nak_on_read_marks_the_image_incomplete() {
    let run = go(
        Faults { nak_at: Some(300), ..lively() },
        Scope::Partitions(vec!["devinfo".into(), "boot_a".into()]),
    );
    let f = run.r.unwrap_err();
    assert!(f.error.to_string().contains("refused read"), "{}", f.error);
    assert!(f.error.to_string().contains("Failed to read sectors"), "{}", f.error);
    assert_eq!(f.restart, Restart::Done);
    assert!(run.mem.files.contains_key("devinfo.img"));
    assert!(run.mem.files.contains_key("boot_a.img.incomplete"));
    // Failed mid-image: never under its final name, nothing left partial.
    assert!(!run.mem.files.contains_key("boot_a.img"));
    assert!(!run.mem.files.keys().any(|k| k.ends_with(".partial")));
    assert!(!run.mem.events.contains(&"keep boot_a".to_string()));
    assert_eq!(run.mem.events.last().unwrap(), "manifest");
    let m: serde_json::Value = serde_json::from_str(f.manifest.as_deref().unwrap()).unwrap();
    assert_eq!(m["disks"].as_array().unwrap().len(), 1);
    assert_eq!(m["incomplete"][0]["name"], "boot_a");
    assert_eq!(m["incomplete"][0]["file"], "boot_a.img.incomplete");
    assert_eq!(commands(&run.dev).last().unwrap(), "power");
}

#[test]
fn short_data_is_an_error_and_a_restart_is_tried() {
    let run = go(
        Faults {
            short_at: Some(100),
            ..lively()
        },
        Scope::Partitions(vec!["devinfo".into()]),
    );
    let f = run.r.unwrap_err();
    assert!(
        matches!(f.error, EmergencyError::Firehose(glassine_installer::firehose::FirehoseError::Short { .. })),
        "{:?}",
        f.error
    );
    assert!(run.mem.files.contains_key("devinfo.img.incomplete"));
    assert_eq!(commands(&run.dev).last().unwrap(), "power");
    assert_eq!(f.restart, Restart::Done);
}

#[test]
fn a_second_read_that_differs_is_a_mismatch() {
    let run = go(
        Faults {
            flip_at: Some(900),
            ..lively()
        },
        Scope::Partitions(vec!["boot_a".into()]),
    );
    let f = run.r.unwrap_err();
    // LBA 900 is in boot_a's second range (sectors 768..1279, 256 KiB in).
    assert_eq!(
        f.error,
        EmergencyError::Mismatch {
            image: "boot_a".into(),
            offset: 256 << 10
        }
    );
    assert!(run.mem.files.contains_key("boot_a.img.incomplete"));
    assert_eq!(f.restart, Restart::Done);
}

#[test]
fn a_stored_copy_that_differs_is_a_mismatch() {
    let mem = Mem {
        corrupt_copy: true,
        ..Default::default()
    };
    let run = go_with(lively(), Scope::Partitions(vec!["devinfo".into()]), mem);
    let f = run.r.unwrap_err();
    assert!(matches!(f.error, EmergencyError::Mismatch { .. }), "{:?}", f.error);
    assert!(run.mem.files.contains_key("devinfo.img.incomplete"));
}

#[test]
fn a_4096_byte_block_size_for_emmc_is_refused() {
    let run = go(Faults { block_size: 4096, ..lively() }, Scope::WholeDisk);
    let f = run.r.unwrap_err();
    assert!(matches!(f.error, EmergencyError::Storage(_)), "{:?}", f.error);
    assert!(reads(&run.dev).is_empty());
    assert_eq!(f.restart, Restart::Done);
}

/// The synthetic disk with its primary GPT damaged: the header's CRC
/// broken (its signature and backup LBA kept), or the head wiped.
fn damaged(f: Faults, wipe: bool) -> Device {
    let mut dev = Device::new(f);
    if wipe {
        dev.disk[..SECTOR * 34].fill(0);
    } else {
        dev.disk[SECTOR + 16] ^= 0xff;
    }
    dev
}

#[test]
fn a_damaged_primary_gpt_is_replaced_by_the_backup() {
    for (info, wipe, what) in [
        (true, true, "size reported"),
        (false, false, "size from the damaged header's backup LBA"),
        (false, true, "size searched for"),
    ] {
        let f = Faults {
            storage_info: info,
            ..lively()
        };
        let run = go_dev(damaged(f, wipe), Scope::Partitions(vec!["devinfo".into()]), Mem::default(), OPTS);
        let done = run.r.unwrap_or_else(|e| panic!("{what}: {e:?}"));
        assert!(!done.table.primary_ok, "{what}");
        assert_eq!(done.table.entries.len(), 3, "{what}");
        assert_eq!(done.disk_size, SECTORS * 512, "{what}");
        assert_eq!(run.mem.files["devinfo.img"], slice(64, 191), "{what}");
        assert!(
            run.mem
                .notes
                .iter()
                .any(|n| n == &format!("using the backup GPT at LBA {}: the primary is damaged", SECTORS - 1)),
            "{what}"
        );
        let r = reads(&run.dev);
        // The head twice, then the backup header and its entries twice each.
        assert_eq!(&r[..2], [(0, 34), (0, 34)], "{what}");
        assert!(r.contains(&(SECTORS - 1, 1)) && r.contains(&(SECTORS - 33, 32)), "{what}: {r:?}");
        let singles = r.iter().filter(|(_, n)| *n == 1).count();
        match what {
            // The backup header twice.
            "size reported" => assert_eq!(singles, 2, "{r:?}"),
            // The hinted LBA once, then twice.
            "size from the damaged header's backup LBA" => assert_eq!(singles, 3, "{r:?}"),
            // Doubling and halving to the end: more.
            _ => {
                assert!(singles > 10, "{r:?}");
                assert!(run.mem.notes.iter().any(|n| n.contains("finding its last sector")));
            }
        }
        assert_eq!(done.restart, Restart::Done);
    }
}

#[test]
fn both_gpts_bad_reads_nothing_more() {
    for info in [true, false] {
        let mut dev = damaged(
            Faults {
                storage_info: info,
                ..lively()
            },
            true,
        );
        let end = SECTORS as usize * SECTOR;
        dev.disk[end - SECTOR..end].fill(0);
        let run = go_dev(dev, Scope::WholeDisk, Mem::default(), OPTS);
        let f = run.r.unwrap_err();
        let e = f.error.to_string();
        assert!(e.contains("no GPT header at LBA 1") && e.contains("the backup GPT isn't at LBA 16383"), "{e}");
        assert!(run.mem.files.is_empty() && run.mem.manifest.is_none());
        assert!(reads(&run.dev).iter().all(|(_, n)| *n <= 34));
        assert_eq!(f.restart, Restart::Done);
    }
}

#[test]
fn an_unknown_partition_reads_nothing() {
    let run = go(lively(), Scope::Partitions(vec!["devinfo".into(), "nosuch".into()]));
    let f = run.r.unwrap_err();
    assert_eq!(f.error, EmergencyError::Partition("no partition named \"nosuch\" in the GPT".into()));
    assert_eq!(reads(&run.dev).len(), 2);
    assert!(run.mem.files.is_empty());
}

#[test]
fn another_chip_gets_nothing() {
    let run = go(
        Faults {
            other_chip: true,
            ..Default::default()
        },
        Scope::GptOnly,
    );
    let f = run.r.unwrap_err();
    assert!(matches!(f.error, EmergencyError::UnknownChip(_)));
    assert!(run.dev.done_at.is_none());
    assert!(!run.dev.sent.contains(&programmer()), "the programmer was sent");
}

#[test]
fn a_refused_programmer_restarts_the_boot_rom() {
    let run = go(
        Faults {
            sahara_refuses: true,
            ..Default::default()
        },
        Scope::GptOnly,
    );
    let f = run.r.unwrap_err();
    assert!(matches!(f.error, EmergencyError::Refused(_)));
    assert!(f.error.to_string().contains("not signed for this chip"), "{}", f.error);
    assert_eq!(f.restart, Restart::Done);
    assert_eq!(run.dev.sent.last().unwrap(), &sahara::reset());
    assert!(run.dev.done_at.is_none());
}

/// The sources themselves: no element of a command that changes anything,
/// one place that sends, and the element names of the requests only the four.
#[test]
fn the_source_has_no_other_command() {
    let files = [
        ("firehose.rs", include_str!("../src/firehose.rs")),
        ("emergency_read.rs", include_str!("../src/emergency_read.rs")),
    ];
    // Put together here, so this file doesn't find itself.
    let verbs = [
        ["pro", "gram"].concat(),
        ["era", "se"].concat(),
        ["pat", "ch"].concat(),
        ["setbootable", "storagedrive"].concat(),
        ["pe", "ek"].concat(),
        ["po", "ke"].concat(),
        ["firmware", "write"].concat(),
        ["ben", "chmark"].concat(),
        ["n", "op"].concat(),
        ["s", "ig"].concat(),
    ];
    for (name, src) in files {
        let code: String = src
            .split("#[cfg(test)]")
            .next()
            .unwrap()
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        for v in &verbs {
            for form in [format!("<{v}"), format!("\"{v}\""), format!("\"{v} ")] {
                assert!(!code.to_lowercase().contains(&form), "{form} in {name}");
            }
        }
        for v in ["setbootable", "firmwarewrite"] {
            assert!(!code.to_lowercase().contains(v), "{v} in {name}");
        }
        let sends = code.matches(".send(").count();
        if name == "firehose.rs" {
            assert_eq!(sends, 1, "every transfer goes through Firehose::request");
            let a = code.find("fn request(").unwrap();
            assert!(code[a..].find(".send(").unwrap() < code[a..].find("\n    }").unwrap());
            let x = &code[code.find("fn xml(&self)").unwrap()..code.find("fn during(&self)").unwrap()];
            let names: Vec<String> = Regex::new(r"<([A-Za-z_]+)").unwrap().captures_iter(x).map(|c| c[1].to_string()).collect();
            assert!(!names.is_empty());
            for n in names {
                assert!(["configure", "getstorageinfo", "read", "power", "data"].contains(&n.as_str()), "<{n} in fn xml");
            }
            assert!(!code.contains("pub enum Request") && !code.contains("pub fn xml"), "the requests are private");
        } else {
            assert_eq!(sends, 0, "{name} sends nothing itself");
            assert!(!code.contains("<?xml") && !code.contains("<data"), "{name} builds no XML");
        }
    }
}

// ---- the device's report before the upload (004 FR-004) ----------------------

/// What the computer sent up to the end of the boot ROM's command mode: the
/// facts' queries and the SWITCH_MODE back, nothing that starts an upload.
fn nothing_uploaded(dev: &Device) {
    assert!(dev.done_at.is_none(), "the upload was finished");
    assert!(!dev.sent.contains(&programmer()), "the programmer was sent");
    for d in &dev.sent {
        let c = u32::from_le_bytes(d[..4].try_into().unwrap());
        assert!(
            [cmd::HELLO_RESP, cmd::EXECUTE, cmd::EXECUTE_DATA, cmd::SWITCH_MODE].contains(&c),
            "Sahara packet {c:#x}"
        );
    }
    let last = dev.sent.last().unwrap();
    assert_eq!(u32::from_le_bytes(last[..4].try_into().unwrap()), cmd::SWITCH_MODE, "something after the facts");
}

/// The described device: the report is compared, said, and the programmer sent.
#[test]
fn the_described_device_gets_the_programmer() {
    let run = go(lively(), Scope::GptOnly);
    let done = run.r.unwrap();
    assert_eq!(done.restart, Restart::Done);
    assert!(run.dev.sent.contains(&programmer()));
    let compared = run
        .mem
        .notes
        .iter()
        .position(|n| n.starts_with("the boot ROM's chip and key hash are "))
        .unwrap();
    let sending = run.mem.notes.iter().position(|n| n.starts_with("sending the programmer")).unwrap();
    assert!(compared < sending);
}

/// A boot ROM that reports another key hash than the description's: nothing
/// is sent, not even a reset; the error says what was reported and expected.
#[test]
fn another_key_hash_gets_nothing() {
    let run = go(Faults { other_key: true, ..lively() }, Scope::GptOnly);
    let f = run.r.unwrap_err();
    let EmergencyError::NotSent(Unsendable::OtherDevice { reported, expected, differs }) = &f.error else {
        panic!("{:?}", f.error)
    };
    assert_eq!(differs, &["pk_hash"]);
    assert_eq!(reported["pk_hash"], hex(&pk_hash(true)));
    assert_eq!(expected["pk_hash"], hex(&pk_hash(false)));
    assert_eq!(reported["chip"], "000cc0e1");
    let e = f.error.to_string();
    assert!(
        e.contains(&hex(&pk_hash(true))) && e.contains(&hex(&pk_hash(false))) && e.contains("nothing was sent"),
        "{e}"
    );
    assert_eq!(f.restart, Restart::NotPossible);
    nothing_uploaded(&run.dev);
}

/// A chip this installer knows, but not the one the description names:
/// nothing is sent.
#[test]
fn a_chip_the_description_doesnt_name_gets_nothing() {
    let d = common::valid(&desc_text().replace("000cc0e1", "000cc0e2"));
    let run = go_desc(Device::new(lively()), &d, Scope::GptOnly, Mem::default(), OPTS);
    let f = run.r.unwrap_err();
    let EmergencyError::NotSent(Unsendable::OtherDevice { differs, expected, .. }) = &f.error else {
        panic!("{:?}", f.error)
    };
    assert_eq!(differs, &["chip"]);
    assert_eq!(expected["chip"], "000cc0e2");
    assert!(f.error.to_string().contains("chip 000cc0e1"), "{}", f.error);
    nothing_uploaded(&run.dev);
}

/// A file the description lists as not working is never sent, even when it
/// reached the route (the command line refuses it first).
#[test]
fn a_known_bad_file_is_not_sent_by_the_route() {
    let t = desc_text().replace(&hex(&Sha256::digest(programmer())), DESC_KNOWN).replace(
        "4c2c494e92551b1d7f06f7beece875c0ef2b9b7b2ffab347f76a3dda8fa3d20c",
        &hex(&Sha256::digest(programmer())),
    );
    let run = go_desc(Device::new(lively()), &common::valid(&t), Scope::GptOnly, Mem::default(), OPTS);
    let f = run.r.unwrap_err();
    assert!(
        matches!(&f.error, EmergencyError::NotSent(Unsendable::File(w)) if w.contains("known not to work")),
        "{:?}",
        f.error
    );
    nothing_uploaded(&run.dev);
}

/// The unchecked entry still runs (a route without the description).
#[test]
fn the_unchecked_run_reads_the_gpt() {
    let mut dev = Device::new(lively());
    let r = block_on(emergency_read::run(&mut dev, &programmer(), &Scope::GptOnly, OPTS, &mut Mem::default()));
    assert_eq!(r.unwrap().table.entries.len(), 3);
}

// ---- where a size came from, and the size search -----------------------------

/// One line per area says its size and where it came from.
#[test]
fn every_size_says_where_it_came_from() {
    for per_partition_info in [true, false] {
        let f = Faults {
            boot_sectors: 640,
            per_partition_info,
            ..lively()
        };
        let run = go(f, Scope::WholeDisk);
        let done = run.r.unwrap();
        let user = format!("emmc-user (physical partition 0): {} bytes, size from getstorageinfo", SECTORS * 512);
        assert!(run.mem.notes.iter().any(|n| n.starts_with(&user)), "{:#?}", run.mem.notes);
        for p in [1, 2] {
            let line = format!("emmc-boot{p} (physical partition {p}): {} bytes, size from ", 640 * 512);
            let n = run
                .mem
                .notes
                .iter()
                .find(|n| n.starts_with(&line))
                .unwrap_or_else(|| panic!("{:#?}", run.mem.notes));
            if per_partition_info {
                assert!(n.ends_with("getstorageinfo (640 sectors; its last sector reads)"), "{n}");
            } else {
                assert!(n.contains("the single-sector search (") && n.contains("answers with the user area's"), "{n}");
            }
        }
        assert_eq!(done.sizes.len(), 3);
        assert_eq!(done.sizes[1].bytes, 640 * 512);
    }
    // Without getstorageinfo the user area's size is the GPT's.
    let run = go(
        Faults {
            storage_info: false,
            ..lively()
        },
        Scope::GptOnly,
    );
    run.r.unwrap();
    let line = format!(
        "emmc-user (physical partition 0): {} bytes, size from the primary GPT header's backup LBA ({})",
        SECTORS * 512,
        SECTORS - 1
    );
    assert!(run.mem.notes.contains(&line), "{:#?}", run.mem.notes);
}

/// `--find-boot-area-sizes` with a programmer that refuses (NAK) reads past
/// the end: both sizes found by the search whatever getstorageinfo says,
/// nothing stored, every search read bounded, the device restarted.
#[test]
fn the_size_search_with_a_programmer_that_refuses() {
    let f = Faults {
        boot_sectors: 640,
        per_partition_info: true,
        ..lively()
    };
    let run = go(f, Scope::BootAreaSizes);
    let done = run.r.unwrap();
    assert_eq!(done.restart, Restart::Done);
    assert!(run.mem.files.is_empty() && run.mem.events.is_empty() && done.manifest.is_none());
    assert_eq!(
        done.sizes.iter().map(|z| (z.physical, z.bytes)).collect::<Vec<_>>(),
        [(0, SECTORS * 512), (1, 640 * 512), (2, 640 * 512)]
    );
    for z in &done.sizes[1..] {
        assert!(z.source.starts_with("the single-sector search ("), "{}", z.source);
        assert!(z.source.contains("refused by the programmer past the end"), "{}", z.source);
        assert!(z.source.ends_with("getstorageinfo reports the same 640 sectors"), "{}", z.source);
    }
    for p in [1, 2] {
        let r = reads_on(&run.dev, p);
        assert!(r.iter().all(|(_, n)| *n == 1), "only single sectors: {r:?}");
        assert!(r.contains(&(639, 1)) && r.contains(&(640, 1)), "{r:?}");
    }
    // Each search read with the short wait, restored after.
    let w = &run.dev.waits;
    assert!(w.contains(&Some(PROBE_WAIT)));
    assert_eq!(
        w.iter().filter(|x| **x == Some(PROBE_WAIT)).count(),
        w.iter().filter(|x| x.is_none()).count() - 1,
        "{w:?}"
    );
    assert_eq!(w.last(), Some(&None));
    assert_eq!(commands(&run.dev).last().unwrap(), "power");
}

/// A programmer that hangs on a read past the end instead of refusing it:
/// the search stops at that read with a clear message, nothing more is read,
/// and the device is still restarted.
#[test]
fn the_size_search_with_a_programmer_that_hangs() {
    let f = Faults {
        boot_sectors: 640,
        silent_past_end: true,
        ..lively()
    };
    let run = go(f, Scope::BootAreaSizes);
    let f = run.r.unwrap_err();
    let EmergencyError::ProbeHangs { image, physical, lba } = &f.error else {
        panic!("{:?}", f.error)
    };
    assert_eq!((image.as_str(), *physical), ("emmc-boot1", 1));
    assert!(*lba >= 640, "{lba}");
    let e = f.error.to_string();
    assert!(
        e.contains("hangs instead of refusing") && e.contains(&format!("within {} s", PROBE_WAIT.as_secs())),
        "{e}"
    );
    assert_eq!(f.restart, Restart::Done);
    // The hung read was the last; then only the restart.
    let c = commands(&run.dev);
    assert_eq!(&c[c.len() - 2..], ["read", "power"]);
    assert_eq!(reads_on(&run.dev, 1).last(), Some(&(*lba, 1)));
    assert!(reads_on(&run.dev, 2).is_empty());
    assert!(run.mem.files.is_empty());
}

/// The same hang in a whole-disk run: the user area stays stored and listed,
/// the boot area isn't, and the device is restarted.
#[test]
fn a_hang_in_the_search_keeps_what_was_read() {
    let f = Faults {
        boot_sectors: 640,
        silent_past_end: true,
        ..lively()
    };
    let run = go(f, Scope::WholeDisk);
    let f = run.r.unwrap_err();
    assert!(matches!(f.error, EmergencyError::ProbeHangs { physical: 1, .. }), "{:?}", f.error);
    assert_eq!(f.restart, Restart::Done);
    assert_eq!(run.mem.files.keys().collect::<Vec<_>>(), ["emmc-user.img"]);
    let m: serde_json::Value = serde_json::from_str(f.manifest.as_deref().unwrap()).unwrap();
    assert_eq!(m["disks"][0]["name"], "emmc-user");
    assert_eq!(m["not_read"][0]["name"], "emmc-boot1");
}

/// A programmer that reads past the end (zeros) never ends the search with
/// a size at the bound: no size is found, and nothing is stored.
#[test]
fn a_programmer_that_never_refuses_gives_no_size() {
    let f = Faults {
        boot_sectors: 640,
        reads_past_end: true,
        ..lively()
    };
    let run = go(f.clone(), Scope::BootAreaSizes);
    let e = run.r.unwrap_err().error.to_string();
    assert!(
        e.contains("no eMMC boot area's size was found") && e.contains("doesn't refuse reads past the end"),
        "{e}"
    );
    let run = go(f, Scope::WholeDisk);
    let done = run.r.unwrap();
    assert!(!run.mem.files.keys().any(|k| k.starts_with("emmc-boot")), "{:?}", run.mem.files.keys());
    let m: serde_json::Value = serde_json::from_str(done.manifest.as_deref().unwrap()).unwrap();
    assert_eq!(m["not_read"].as_array().unwrap().len(), 2);
}
