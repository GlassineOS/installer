// SPDX-License-Identifier: GPL-2.0-or-later
//! The computer's side of Firehose, the XML protocol a Qualcomm programmer
//! speaks once the boot ROM has started it (spec 004). The programmer is the
//! person's own file, sent over Sahara ([`crate::sahara::Sahara::upload`]);
//! this module only talks to it afterwards.
//!
//! Read-only by construction (004 FR-007): the only requests this module can
//! build are the four of [`Request`], and it is private. `configure` sets the
//! session up, `getstorageinfo` lists the storage, `read` reads sectors and
//! `power` with the value `reset` restarts the device. There is no way to
//! send any other element, an attribute a caller chose, or raw bytes to the
//! device: every transfer to it goes through [`Firehose::request`], which
//! serialises a [`Request`]. The tests record every transfer of a whole
//! session and check each against that list, and read this file's source for
//! the element names of every other command.
//!
//! The wire, as the public sources (the `edl` tool, Linux's `qdl`) describe
//! it: the computer sends one XML document per request,
//! `<?xml …?><data><NAME attr="…"/></data>`; the programmer answers with any
//! number of documents holding `<log value="…"/>` and then one holding
//! `<response value="ACK"|"NAK" …/>`. A `read` is answered by a response with
//! `rawmode="true"`, then exactly the sectors asked for as raw bytes, then a
//! second response (`rawmode="false"`) that says whether the read went well.
//!
//! The receiver treats what the device sends as a byte stream, like the
//! Sahara one: several documents in one transfer, a document split over
//! transfers, raw bytes in the same transfer as the response before them,
//! and empty transfers (a zero-length packet at the end of a transfer that
//! fills whole USB packets, which a host that says it is ZLP-aware gets) all
//! read the same. No clocks here: every wait is bounded by the transport's
//! timeout and by counts (documents per answer, empty transfers in a row).

use crate::fastboot::{FbError, Transport};
use std::fmt;

/// The storage this client configures. The Note Air's is eMMC; UFS and the
/// others are not supported (their sector size and their physical
/// partitions differ, and nothing here was checked against them).
pub const MEMORY_NAME: &str = "eMMC";

/// eMMC's sector size. The programmer is asked for its block size first
/// ([`StorageInfo`]); when it doesn't say, this is checked by finding the GPT
/// header at LBA 1. 4096 is never tried for eMMC.
pub const EMMC_SECTOR: u32 = 512;

/// What the computer offers as the largest payload it sends in one piece.
/// Only writes use it (and this client has none), but the programmer
/// expects it in `configure`, and may answer with the size it supports
/// ([`Firehose::start`]).
pub const PAYLOAD_TO_TARGET: u64 = 1 << 20;

/// The most one `read` request asks for. Each read is its own request with
/// its own closing answer, so a failure is known within this many bytes.
pub const READ_CHUNK: u64 = 4 << 20;

/// The most one bulk IN transfer is asked for in the raw phase.
pub const TRANSFER: usize = 1 << 20;

/// What one bulk IN transfer is asked for while documents are expected.
const XML_RECV: usize = 4096;
/// The longest document the receiver keeps.
const DOC_MAX: usize = 64 << 10;
/// Empty transfers in a row before the device counts as gone.
const EMPTY_MAX: u32 = 16;
/// Documents (logs) before the response; more is a programmer that talks
/// and never answers.
const DOCS_MAX: usize = 4096;
/// Log lines kept for an error message.
const LOGS_KEPT: usize = 32;

/// The four requests. Private on purpose: see the module's documentation.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Request {
    /// `configure`, offering `payload` as [`PAYLOAD_TO_TARGET`].
    Configure { payload: u64 },
    /// `getstorageinfo` of one physical partition.
    GetStorageInfo { physical: u32 },
    /// `read`: `sectors` sectors of `sector` bytes from `start`.
    Read { sector: u32, sectors: u64, physical: u32, start: u64 },
    /// `power` with the value `reset`, and nothing else.
    PowerReset,
}

impl Request {
    /// The XML document. Every attribute value is a number or a constant of
    /// this file; nothing from a caller or the device is put in.
    ///
    /// `configure` follows what `edl` sends; `ZLPAwareHost` is `edl`'s
    /// spelling (`qdl` writes `ZlpAwareHost`; which one a given programmer
    /// reads is not known here). `power` sends only `value="reset"`, as
    /// `edl` does; `qdl` adds `DelayInSeconds="10"`.
    fn xml(&self) -> String {
        let body = match self {
            Request::Configure { payload } => format!(
                "<configure MemoryName=\"{MEMORY_NAME}\" Verbose=\"0\" AlwaysValidate=\"0\" MaxDigestTableSizeInBytes=\"2048\" \
                 MaxPayloadSizeToTargetInBytes=\"{payload}\" ZLPAwareHost=\"1\" SkipStorageInit=\"0\" />"
            ),
            Request::GetStorageInfo { physical } => format!("<getstorageinfo physical_partition_number=\"{physical}\" />"),
            Request::Read {
                sector,
                sectors,
                physical,
                start,
            } => format!(
                "<read SECTOR_SIZE_IN_BYTES=\"{sector}\" num_partition_sectors=\"{sectors}\" \
                 physical_partition_number=\"{physical}\" start_sector=\"{start}\" />"
            ),
            Request::PowerReset => "<power value=\"reset\" />".to_string(),
        };
        format!("<?xml version=\"1.0\" encoding=\"UTF-8\" ?><data>{body}</data>")
    }

    fn during(&self) -> &'static str {
        match self {
            Request::Configure { .. } => "configure",
            Request::GetStorageInfo { .. } => "getstorageinfo",
            Request::Read { .. } => "read",
            Request::PowerReset => "power reset",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirehoseError {
    Transport(FbError),
    /// Nothing came for the first request (004 FR-006): the programmer
    /// doesn't work on this device, or isn't a Firehose programmer.
    NoAnswer,
    /// The programmer refused a request. `logs` are its log lines before the
    /// answer (with anything that names the unit left out).
    Nak {
        during: &'static str,
        logs: Vec<String>,
    },
    /// The programmer wants an authentication first, which this installer
    /// doesn't do.
    NeedsAuth {
        logs: Vec<String>,
    },
    /// What the programmer sent isn't a document this client understands.
    Malformed(String),
    /// A valid answer at the wrong time.
    Unexpected(String),
    /// The raw data of a read stopped early.
    Short {
        want: u64,
        got: u64,
    },
    /// Not something this client does (a range it won't ask for).
    Refused(String),
    /// The stream is out of step after an earlier failure.
    Broken,
}

impl fmt::Display for FirehoseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let logs = |l: &[String]| {
            if l.is_empty() {
                String::new()
            } else {
                format!(" (it logged: {})", l.join(" | "))
            }
        };
        match self {
            FirehoseError::Transport(e) => write!(f, "{e}"),
            FirehoseError::NoAnswer => write!(f, "the programmer didn't answer its first request"),
            FirehoseError::Nak { during, logs: l } => write!(f, "the programmer refused {during}{}", logs(l)),
            FirehoseError::NeedsAuth { logs: l } => {
                write!(f, "the programmer asks for an authentication first, which this installer doesn't do{}", logs(l))
            }
            FirehoseError::Malformed(w) => write!(f, "the programmer sent something that isn't a Firehose answer: {w}"),
            FirehoseError::Unexpected(w) => write!(f, "unexpected answer: {w}"),
            FirehoseError::Short { want, got } => write!(f, "the read's data stopped after {got} of {want} bytes"),
            FirehoseError::Refused(w) => write!(f, "refused: {w}"),
            FirehoseError::Broken => write!(f, "the connection is out of step after an earlier failure"),
        }
    }
}

impl std::error::Error for FirehoseError {}

impl From<FbError> for FirehoseError {
    fn from(e: FbError) -> Self {
        FirehoseError::Transport(e)
    }
}

/// One element of a document: its name and attributes, entities decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
}

impl Element {
    pub fn attr(&self, k: &str) -> Option<&str> {
        self.attrs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
    }
}

fn unescape(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The elements of one document (`<?xml …?><data>…</data>`), in order, the
/// declaration and `data` itself left out. Tolerant where programmers are
/// known to be loose: a `log` value runs to the last quote before its `/>`,
/// so quotes in it (the JSON of `getstorageinfo`) needn't be escaped.
pub fn parse_document(doc: &str) -> Result<Vec<Element>, String> {
    let mut out = Vec::new();
    let mut rest = doc;
    while let Some(i) = rest.find('<') {
        rest = &rest[i + 1..];
        if let Some(r) = rest.strip_prefix("!--") {
            rest = r.find("-->").map(|e| &r[e + 3..]).ok_or("an unterminated comment")?;
            continue;
        }
        if rest.starts_with('?') || rest.starts_with('/') {
            rest = rest.find('>').map(|e| &rest[e + 1..]).ok_or("an unterminated tag")?;
            continue;
        }
        let name_end = rest.find(|c: char| c.is_whitespace() || c == '/' || c == '>').ok_or("an unterminated tag")?;
        let name = &rest[..name_end];
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(format!("an element named {name:?}"));
        }
        rest = &rest[name_end..];
        if name == "data" {
            rest = rest.find('>').map(|e| &rest[e + 1..]).ok_or("an unterminated data tag")?;
            continue;
        }
        if name == "log" {
            // value="…" up to the last quote before the element's end.
            let end = rest.find("/>").ok_or("an unterminated log")?;
            let body = &rest[..end];
            let value = body
                .find("value=\"")
                .and_then(|v| {
                    let from = v + 7;
                    body[from..].rfind('"').map(|q| &body[from..from + q])
                })
                .ok_or("a log without a value")?;
            out.push(Element {
                name: name.into(),
                attrs: vec![("value".into(), unescape(value))],
            });
            rest = &rest[end + 2..];
            continue;
        }
        let mut attrs = Vec::new();
        loop {
            rest = rest.trim_start();
            if let Some(r) = rest.strip_prefix("/>").or_else(|| rest.strip_prefix('>')) {
                rest = r;
                break;
            }
            let eq = rest.find('=').ok_or("an attribute without a value")?;
            let k = rest[..eq].trim();
            if k.is_empty() || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
                return Err(format!("an attribute named {k:?}"));
            }
            let r = rest[eq + 1..].trim_start();
            let q = r.chars().next().filter(|c| *c == '"' || *c == '\'').ok_or("an unquoted attribute")?;
            let close = r[1..].find(q).ok_or("an unterminated attribute")?;
            attrs.push((k.to_string(), unescape(&r[1..1 + close])));
            rest = &r[close + 2..];
        }
        out.push(Element { name: name.into(), attrs });
    }
    Ok(out)
}

/// What the programmer said about the storage. Its answer also names the
/// chip's serial and the eMMC's: those are never kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StorageInfo {
    pub total_blocks: Option<u64>,
    pub block_size: Option<u32>,
    pub num_physical: Option<u32>,
    pub mem_type: Option<String>,
}

/// The storage facts from the log lines of a `getstorageinfo` answer:
/// `INFO: {"storage_info": {...}}`, the form both public sources read.
pub fn parse_storage_info(logs: &[String]) -> Option<StorageInfo> {
    for l in logs {
        if !l.contains("storage_info") {
            continue;
        }
        let (Some(a), Some(b)) = (l.find('{'), l.rfind('}')) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&l[a..=b]) else {
            continue;
        };
        let s = &v["storage_info"];
        if !s.is_object() {
            continue;
        }
        let num = |k: &str| s[k].as_u64().or_else(|| s[k].as_str().and_then(|t| t.parse().ok()));
        return Some(StorageInfo {
            total_blocks: num("total_blocks"),
            block_size: num("block_size").and_then(|n| u32::try_from(n).ok()),
            num_physical: num("num_physical").and_then(|n| u32::try_from(n).ok()),
            mem_type: s["mem_type"].as_str().map(str::to_string),
        });
    }
    None
}

/// The log lines that may be shown: storage facts and anything naming a
/// serial are left out (they are unique to the unit).
pub fn shown_logs(logs: &[String]) -> Vec<String> {
    let kept: Vec<String> = logs
        .iter()
        .filter(|l| {
            let low = l.to_ascii_lowercase();
            !low.contains("serial") && !low.contains("storage_info")
        })
        .cloned()
        .collect();
    kept[kept.len().saturating_sub(LOGS_KEPT)..].to_vec()
}

/// The programmer's answer to a request: the response and the logs before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub response: Element,
    pub logs: Vec<String>,
}

impl Answer {
    pub fn ack(&self) -> bool {
        self.response.attr("value") == Some("ACK")
    }
    pub fn rawmode(&self) -> bool {
        self.response.attr("rawmode").is_some_and(|v| v.eq_ignore_ascii_case("true"))
    }
}

/// What `configure` settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Configured {
    /// The payload size the programmer took.
    pub payload_to_target: u64,
    /// Whether it asked for another size first (and was sent `configure`
    /// again with it).
    pub renegotiated: bool,
    pub payload_from_target: Option<u64>,
    pub max_xml: Option<u64>,
    /// Its name for the chip, when it gives one.
    pub target_name: Option<String>,
    pub version: Option<String>,
    /// Its log lines up to the answer, as [`shown_logs`] lets them be shown.
    pub logs: Vec<String>,
}

fn needs_auth(logs: &[String]) -> bool {
    logs.iter()
        .any(|l| l.contains("Only nop and sig tag") || l.to_ascii_lowercase().contains("authenticat"))
}

/// One Firehose session.
pub struct Firehose<T: Transport> {
    t: T,
    rx: Vec<u8>,
    configured: Option<Configured>,
    broken: bool,
    /// What [`Firehose::listen`] read.
    early_logs: Vec<String>,
}

impl<T: Transport> Firehose<T> {
    /// A session over the transport of a programmer the boot ROM has just
    /// started. Nothing is sent until [`Firehose::configure`].
    pub fn new(t: T) -> Self {
        Firehose {
            t,
            rx: Vec::new(),
            configured: None,
            broken: false,
            early_logs: Vec::new(),
        }
    }

    /// Reads what the programmer sends on its own once it starts (log
    /// lines), until the transport's first timeout, and sends nothing. Some
    /// programmers take requests only after these lines (`qdl` reads them
    /// before its first `configure`, `edl` drains them too), so a
    /// `configure` sent at once could go unanswered. A timeout ends the
    /// listening and isn't an error; on a transport that closes after its
    /// first timeout (the browser's) this must not be called. The lines go
    /// into [`Configured::logs`] as [`shown_logs`] lets them be shown.
    pub async fn listen(&mut self) -> Result<usize, FirehoseError> {
        if self.configured.is_some() || self.broken {
            return Err(FirehoseError::Refused("listening is before configure".into()));
        }
        for _ in 0..DOCS_MAX {
            let elements = match self.document().await {
                Ok(e) => e,
                Err(FirehoseError::Transport(FbError::Timeout(_))) if self.rx.is_empty() => return Ok(self.early_logs.len()),
                Err(e) => return Err(e),
            };
            for e in elements {
                match e.name.as_str() {
                    "log" => self.early_logs.push(e.attr("value").unwrap_or_default().to_string()),
                    "response" => {
                        self.broken = true;
                        return Err(FirehoseError::Unexpected("a response before any request".into()));
                    }
                    _ => {}
                }
            }
        }
        self.broken = true;
        Err(FirehoseError::Unexpected(format!("{DOCS_MAX} documents and no pause")))
    }

    /// Sends `configure` and waits for the programmer's first answer (004
    /// FR-006): until it has answered, it doesn't count as working and
    /// nothing is read. `configure` is sent once and not again on a timeout
    /// (a second one could leave two answers queued, and a browser's
    /// transport is closed after its first timeout); the transport's
    /// timeout is the wait. Logs the programmer sent on its own before are
    /// read as part of the answer. A timeout before any byte came is
    /// [`FirehoseError::NoAnswer`].
    ///
    /// When the programmer answers NAK and names the payload size it takes
    /// (`MaxPayloadSizeToTargetInBytesSupported`, else
    /// `MaxPayloadSizeToTargetInBytes`), `configure` is sent once more with
    /// that size, and only an ACK goes on. After a NAK the session can still
    /// [`Firehose::reset`].
    pub async fn configure(&mut self) -> Result<&Configured, FirehoseError> {
        if self.configured.is_some() || self.broken {
            return Err(FirehoseError::Refused("configure is sent once, first".into()));
        }
        let first = Request::Configure { payload: PAYLOAD_TO_TARGET };
        self.request(&first).await?;
        let mut a = match self.answer(&first).await {
            Err(FirehoseError::Transport(FbError::Timeout(_))) if self.rx.is_empty() => {
                self.broken = true;
                return Err(FirehoseError::NoAnswer);
            }
            r => r?,
        };
        let mut c = Configured {
            payload_to_target: PAYLOAD_TO_TARGET,
            renegotiated: false,
            payload_from_target: None,
            max_xml: None,
            target_name: None,
            version: None,
            logs: vec![],
        };
        let mut logs = std::mem::take(&mut self.early_logs);
        logs.extend(a.logs.iter().cloned());
        if !a.ack() {
            if needs_auth(&logs) {
                return Err(FirehoseError::NeedsAuth { logs: shown_logs(&logs) });
            }
            let size = ["MaxPayloadSizeToTargetInBytesSupported", "MaxPayloadSizeToTargetInBytes"]
                .iter()
                .find_map(|k| a.response.attr(k).and_then(|v| v.parse::<u64>().ok()))
                .filter(|&n| n > 0 && n != PAYLOAD_TO_TARGET);
            let Some(size) = size else {
                return Err(FirehoseError::Nak {
                    during: first.during(),
                    logs: shown_logs(&a.logs),
                });
            };
            let again = Request::Configure { payload: size };
            self.request(&again).await?;
            a = self.answer(&again).await?;
            logs.extend(a.logs.iter().cloned());
            if !a.ack() {
                return Err(FirehoseError::Nak {
                    during: again.during(),
                    logs: shown_logs(&a.logs),
                });
            }
            c.payload_to_target = size;
            c.renegotiated = true;
        }
        let num = |k: &str| a.response.attr(k).and_then(|v| v.parse::<u64>().ok());
        if let Some(n) = num("MaxPayloadSizeToTargetInBytes") {
            c.payload_to_target = n;
        }
        c.payload_from_target = num("MaxPayloadSizeFromTargetInBytes");
        c.max_xml = num("MaxXMLSizeInBytes");
        c.target_name = a.response.attr("TargetName").map(str::to_string);
        c.version = a.response.attr("Version").map(str::to_string);
        c.logs = shown_logs(&logs);
        Ok(self.configured.insert(c))
    }

    /// What `configure` settled, once it did.
    pub fn configured(&self) -> Option<&Configured> {
        self.configured.as_ref()
    }

    /// The one place that sends anything to the programmer.
    async fn request(&mut self, r: &Request) -> Result<(), FirehoseError> {
        self.t.send(r.xml().as_bytes()).await?;
        Ok(())
    }

    /// One more transfer into the buffer; empty transfers (ZLPs) are
    /// skipped, up to [`EMPTY_MAX`] in a row.
    async fn more(&mut self, max: usize) -> Result<(), FirehoseError> {
        for _ in 0..EMPTY_MAX {
            let b = self.t.recv_max(max).await?;
            if !b.is_empty() {
                self.rx.extend_from_slice(&b);
                return Ok(());
            }
        }
        Err(FirehoseError::Transport(FbError::Usb(format!("{EMPTY_MAX} empty transfers in a row"))))
    }

    /// The next whole document's elements.
    async fn document(&mut self) -> Result<Vec<Element>, FirehoseError> {
        loop {
            let lead = self.rx.iter().take_while(|b| b.is_ascii_whitespace() || **b == 0).count();
            self.rx.drain(..lead);
            if let Some(&b) = self.rx.first()
                && b != b'<'
            {
                self.broken = true;
                return Err(FirehoseError::Malformed(format!("bytes that don't start a document ({b:#04x})")));
            }
            if let Some(end) = find(&self.rx, b"</data>") {
                let doc: Vec<u8> = self.rx.drain(..end + 7).collect();
                let text = String::from_utf8_lossy(&doc);
                return parse_document(&text).map_err(|w| {
                    self.broken = true;
                    FirehoseError::Malformed(w)
                });
            }
            if self.rx.len() > DOC_MAX {
                self.broken = true;
                return Err(FirehoseError::Malformed(format!("a document longer than {DOC_MAX} bytes")));
            }
            self.more(XML_RECV).await?;
        }
    }

    /// Documents until one holds a response; the logs before it are kept.
    async fn answer(&mut self, r: &Request) -> Result<Answer, FirehoseError> {
        let mut logs = Vec::new();
        for _ in 0..DOCS_MAX {
            for e in self.document().await? {
                match e.name.as_str() {
                    "log" => logs.push(e.attr("value").unwrap_or_default().to_string()),
                    "response" => match e.attr("value") {
                        Some("ACK" | "NAK") => return Ok(Answer { response: e, logs }),
                        v => {
                            self.broken = true;
                            return Err(FirehoseError::Unexpected(format!("a response {v:?} to {}", r.during())));
                        }
                    },
                    // Other elements a programmer may add: ignored.
                    _ => {}
                }
            }
        }
        self.broken = true;
        Err(FirehoseError::Unexpected(format!("{DOCS_MAX} documents and no response to {}", r.during())))
    }

    fn check(&self) -> Result<(), FirehoseError> {
        if self.broken {
            Err(FirehoseError::Broken)
        } else if self.configured.is_none() {
            Err(FirehoseError::Refused("configure first".into()))
        } else {
            Ok(())
        }
    }

    /// The storage facts of one physical partition, `None` when the
    /// programmer doesn't answer `getstorageinfo` with them (older ones
    /// refuse the request).
    pub async fn storage_info(&mut self, physical: u32) -> Result<Option<StorageInfo>, FirehoseError> {
        self.check()?;
        let r = Request::GetStorageInfo { physical };
        self.request(&r).await?;
        let a = self.answer(&r).await?;
        Ok(if a.ack() { parse_storage_info(&a.logs) } else { None })
    }

    /// Reads `sectors` sectors of `sector` bytes from `start` of physical
    /// partition `physical` and hands the bytes to `sink` in order, in
    /// requests of at most `chunk` bytes (a multiple of `sector`).
    pub async fn read(
        &mut self,
        physical: u32,
        sector: u32,
        start: u64,
        sectors: u64,
        chunk: u64,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), FirehoseError> {
        self.check()?;
        let s = sector as u64;
        if sector == 0 || !chunk.is_multiple_of(s) || chunk < s || sectors == 0 || start.checked_add(sectors).is_none() {
            return Err(FirehoseError::Refused(format!(
                "a read of {sectors} sectors of {sector} bytes in chunks of {chunk}"
            )));
        }
        let per = chunk / s;
        let mut at = start;
        let end = start + sectors;
        while at < end {
            let n = per.min(end - at);
            self.read_one(physical, sector, at, n, sink).await?;
            at += n;
        }
        Ok(())
    }

    async fn read_one(
        &mut self,
        physical: u32,
        sector: u32,
        start: u64,
        sectors: u64,
        sink: &mut dyn FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<(), FirehoseError> {
        let r = Request::Read {
            sector,
            sectors,
            physical,
            start,
        };
        self.request(&r).await?;
        let a = self.answer(&r).await?;
        if !a.ack() {
            return Err(FirehoseError::Nak {
                during: r.during(),
                logs: shown_logs(&a.logs),
            });
        }
        if !a.rawmode() {
            // An ACK without data: nothing to read, and the stream is unclear.
            self.broken = true;
            return Err(FirehoseError::Unexpected("an ACK to read without rawmode".into()));
        }
        let want = sectors * sector as u64;
        let mut got = 0u64;
        while got < want {
            if self.rx.is_empty() {
                let left = (want - got).min(TRANSFER as u64) as usize;
                match self.more(left).await {
                    Ok(()) => {}
                    Err(FirehoseError::Transport(FbError::Timeout(_))) => {
                        self.broken = true;
                        return Err(FirehoseError::Short { want, got });
                    }
                    Err(e) => {
                        self.broken = true;
                        return Err(e);
                    }
                }
            }
            // Bytes past the data (the closing answer, should a transfer
            // carry both) stay in the buffer.
            let n = self.rx.len().min((want - got) as usize);
            let data: Vec<u8> = self.rx.drain(..n).collect();
            sink(&data).map_err(|e| {
                self.broken = true;
                FirehoseError::Refused(format!("storing the read failed: {e}"))
            })?;
            got += n as u64;
        }
        let end = self.answer(&r).await?;
        if !end.ack() {
            return Err(FirehoseError::Nak {
                during: "the end of a read",
                logs: shown_logs(&end.logs),
            });
        }
        if end.rawmode() {
            self.broken = true;
            return Err(FirehoseError::Unexpected("a second rawmode answer after a read's data".into()));
        }
        Ok(())
    }

    /// [`Firehose::read`] into memory, for small reads (the GPT).
    pub async fn read_vec(&mut self, physical: u32, sector: u32, start: u64, sectors: u64) -> Result<Vec<u8>, FirehoseError> {
        let mut v = Vec::new();
        self.read(physical, sector, start, sectors, READ_CHUNK, &mut |d| {
            v.extend_from_slice(d);
            Ok(())
        })
        .await?;
        Ok(v)
    }

    /// `power` with `reset`: the device restarts. Tried even after an
    /// earlier failure (what was left in the buffer is dropped); `Ok` when
    /// the programmer answered ACK.
    pub async fn reset(&mut self) -> Result<(), FirehoseError> {
        self.rx.clear();
        let r = Request::PowerReset;
        self.request(&r).await?;
        let a = self.answer(&r).await?;
        self.broken = true;
        if a.ack() {
            Ok(())
        } else {
            Err(FirehoseError::Nak {
                during: r.during(),
                logs: shown_logs(&a.logs),
            })
        }
    }

    pub fn into_transport(self) -> T {
        self.t
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents() {
        let d = r#"<?xml version="1.0" encoding="UTF-8" ?><data><log value="INFO: {"storage_info": {"total_blocks":10}}" /></data>"#;
        let e = parse_document(d).unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].attr("value"), Some(r#"INFO: {"storage_info": {"total_blocks":10}}"#));
        let d = r#"<?xml version="1.0" ?><data><response value="ACK" rawmode="true" MaxPayloadSizeFromTargetInBytes='4096' Name="a &amp; b"/></data>"#;
        let e = parse_document(d).unwrap();
        assert_eq!(e[0].name, "response");
        assert_eq!(e[0].attr("rawmode"), Some("true"));
        assert_eq!(e[0].attr("MaxPayloadSizeFromTargetInBytes"), Some("4096"));
        assert_eq!(e[0].attr("Name"), Some("a & b"));
        assert!(parse_document("<data><response value=ACK/></data>").is_err());
        assert!(parse_document("<data><log/></data>").is_err());
        assert!(parse_document("<data><a b=\"c/></data>").is_err());
        let e = parse_document("<data><!-- x --><log value=\"a &lt; b\"/><log value=\"\" /></data>").unwrap();
        assert_eq!(e[0].attr("value"), Some("a < b"));
        assert_eq!(e[1].attr("value"), Some(""));
    }

    #[test]
    fn storage_info_and_redaction() {
        let logs = vec![
            "INFO: Calling handler for getstorageinfo".to_string(),
            r#"INFO: {"storage_info": {"total_blocks":61071360, "block_size":512, "page_size":512, "num_physical":4, "serial_num":1234, "mem_type":"eMMC"}}"#
                .to_string(),
        ];
        let s = parse_storage_info(&logs).unwrap();
        assert_eq!(s.total_blocks, Some(61071360));
        assert_eq!(s.block_size, Some(512));
        assert_eq!(s.num_physical, Some(4));
        assert_eq!(s.mem_type.as_deref(), Some("eMMC"));
        assert_eq!(shown_logs(&logs), vec![logs[0].clone()]);
        assert!(parse_storage_info(&logs[..1]).is_none());
        assert!(shown_logs(&["Serial number 1".to_string()]).is_empty());
    }

    #[test]
    fn requests() {
        let r = Request::Read {
            sector: 512,
            sectors: 34,
            physical: 0,
            start: 0,
        };
        let x = r.xml();
        assert!(x.starts_with("<?xml"));
        let e = parse_document(&x).unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].name, "read");
        assert_eq!(e[0].attr("num_partition_sectors"), Some("34"));
        let e = parse_document(&Request::PowerReset.xml()).unwrap();
        assert_eq!((e[0].name.as_str(), e[0].attr("value")), ("power", Some("reset")));
        let e = parse_document(&Request::Configure { payload: 4096 }.xml()).unwrap();
        assert_eq!(e[0].attr("MemoryName"), Some("eMMC"));
        assert_eq!(e[0].attr("MaxPayloadSizeToTargetInBytes"), Some("4096"));
    }
}
