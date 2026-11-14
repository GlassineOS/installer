// SPDX-License-Identifier: GPL-2.0-or-later
//! The emergency read route against a simulated device: a scripted boot ROM
//! (Sahara), then a simulated programmer that answers Firehose requests the
//! way the public sources describe a real one (logs, ACK and NAK, a NAK
//! naming the payload size it takes, rawmode data, a closing answer), over a
//! synthetic eMMC with a GPT. Every transfer the computer sends is recorded:
//! after the programmer starts, each must be an XML document whose one
//! command is configure, getstorageinfo, read or power reset (004 FR-007).
//! No serial, key, programmer or disk of a real unit is in here.

use glassine_installer::emergency_read::{self, Done, EmergencyError, Failure, ImageEdge, Options, Restart, Scope};
use glassine_installer::fastboot::{FbError, Transport};
use glassine_installer::glassine_read::Disk;
use glassine_installer::sahara::{self, cmd};
use glassine_installer::table::crc32;
use pollster::block_on;
use regex_lite::Regex;
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
};

/// The partitions of the synthetic disk: name, first and last LBA.
const PARTS: &[(&str, u64, u64)] = &[("devinfo", 64, 191), ("boot_a", 256, 2303), ("userdata", 4096, 16350)];

fn programmer() -> Vec<u8> {
    // Stand-in bytes: not a program.
    (0..0x40u32).map(|i| (i * 13) as u8).collect()
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
    d
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
}

struct Device {
    disk: Vec<u8>,
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
}

fn doc(body: &str) -> Vec<u8> {
    format!("<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<data>\n{body}\n</data>").into_bytes()
}

fn log(v: &str) -> Vec<u8> {
    doc(&format!("<log value=\"{v}\" />"))
}

impl Device {
    fn new(f: Faults) -> Self {
        let hash: Vec<u8> = (0u8..32).map(|b| b ^ 0x3c).collect();
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
        Device {
            disk: disk(),
            f,
            out,
            sent: vec![],
            done_at: None,
            reads_covering: BTreeMap::new(),
            stuck: false,
            deaf_pops: 0,
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
        match name.as_str() {
            "configure" => {
                assert_eq!(attrs.get("MemoryName").map(String::as_str), Some("eMMC"));
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
                    self.push(vec![doc(&r)]);
                }
            }
            "getstorageinfo" => {
                if self.f.storage_info {
                    // Unescaped quotes, and a serial: what programmers send.
                    let j = format!(
                        r#"INFO: {{"storage_info": {{"total_blocks":{SECTORS}, "block_size":{}, "page_size":512, "num_physical":4, "serial_num":305419896, "fw_version":"1", "mem_type":"eMMC", "prod_name":"SYNTH"}}}}"#,
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
                assert_eq!(num("physical_partition_number"), 0);
                let (start, n) = (num("start_sector"), num("num_partition_sectors"));
                assert!(n > 0 && start + n <= SECTORS, "a read past the disk: {text}");
                let covers = |at: Option<u64>| at.is_some_and(|a| a >= start && a < start + n);
                if covers(self.f.nak_at) {
                    self.f.nak_at = None;
                    self.push(vec![log("ERROR: Failed to read sectors"), doc("<response value=\"NAK\" />")]);
                    return;
                }
                let mut data = self.disk[start as usize * SECTOR..(start + n) as usize * SECTOR].to_vec();
                let times = self.reads_covering.entry(start).or_default();
                *times += 1;
                if let Some(a) = self.f.flip_at.filter(|a| *a >= start && *a < start + n)
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

/// Stores the images in memory.
#[derive(Default)]
struct Mem {
    files: BTreeMap<String, Vec<u8>>,
    writing: Option<String>,
    reading: Option<(Vec<u8>, usize)>,
    manifest: Option<String>,
    notes: Vec<String>,
    /// The stored copy reads back one byte different.
    corrupt_copy: bool,
}

impl ImageEdge for Mem {
    fn begin(&mut self, d: &Disk) -> Result<(), String> {
        let n = format!("{}.img", d.name);
        assert!(!self.files.contains_key(&n));
        self.files.insert(n.clone(), vec![]);
        self.writing = Some(n);
        Ok(())
    }
    fn write(&mut self, data: &[u8]) -> Result<(), String> {
        let n = self.writing.clone().ok_or("nothing begun")?;
        self.files.get_mut(&n).unwrap().extend_from_slice(data);
        Ok(())
    }
    fn end(&mut self, _: &Disk) -> Result<String, String> {
        self.writing.take().ok_or("nothing begun".into())
    }
    fn reopen(&mut self, d: &Disk) -> Result<(), String> {
        let mut b = self.files[&format!("{}.img", d.name)].clone();
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
        Ok(n)
    }
    fn incomplete(&mut self, d: &Disk) -> Result<String, String> {
        self.writing = None;
        let n = format!("{}.img", d.name);
        let b = self.files.remove(&n).ok_or("no such copy")?;
        let m = format!("{n}.incomplete");
        self.files.insert(m.clone(), b);
        Ok(m)
    }
    fn manifest(&mut self, text: &str) -> Result<(), String> {
        assert!(self.manifest.is_none());
        self.manifest = Some(text.to_string());
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
    let mut dev = Device::new(f);
    let mut mem = mem;
    let r = block_on(emergency_read::run(&mut dev, &programmer(), &scope, opts, &mut mem));
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
    let Some(at) = dev.done_at else { return vec![] };
    let re = Regex::new(r#"num_partition_sectors="(\d+)" physical_partition_number="0" start_sector="(\d+)""#).unwrap();
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
    // Without listening first, configure goes unanswered: what a browser,
    // which can't listen, would meet with such a programmer.
    let run = go_opts(f, Scope::GptOnly, Mem::default(), Options { listen_first: false, ..OPTS });
    let e = run.r.unwrap_err();
    assert!(matches!(e.error, EmergencyError::NotWorking(_)), "{:?}", e.error);
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
    assert!(matches!(f.error, EmergencyError::NotWorking(_)), "{:?}", f.error);
    assert!(f.error.to_string().contains("isn't working on this device"));
    assert_eq!(f.restart, Restart::NotPossible);
    // One configure, nothing read, nothing more.
    assert_eq!(commands(&run.dev), ["configure"]);
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
    assert!(!run.mem.files.contains_key("boot_a.img"));
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

#[test]
fn no_gpt_reads_nothing_more() {
    let mut dev = Device::new(lively());
    dev.disk[..SECTOR * 34].fill(0);
    let mut mem = Mem::default();
    let r = block_on(emergency_read::run(&mut dev, &programmer(), &Scope::WholeDisk, OPTS, &mut mem));
    whitelist(&dev);
    let f = r.unwrap_err();
    assert!(f.error.to_string().contains("no GPT header at LBA 1"), "{}", f.error);
    assert_eq!(reads(&dev).len(), 2);
    assert!(mem.files.is_empty());
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
