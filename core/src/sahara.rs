// SPDX-License-Identifier: GPL-2.0-or-later
//! The computer's side of Sahara, the protocol a Qualcomm boot ROM speaks in
//! its emergency download mode (USB 05c6:9008, "EDL"), spec 004.
//!
//! The boot ROM opens with HELLO. The computer answers HELLO_RESP with one of
//! two modes:
//! - command mode: the boot ROM answers CMD_READY and then runs read-only
//!   queries (EXECUTE): the serial number, the MSM hardware id, the hash of the
//!   OEM's root key, the boot ROM's version. [`Sahara::read_facts`] reads them
//!   into [`EmergencyFacts`] before anything is sent to the device;
//! - image transfer: the boot ROM asks for the program in pieces
//!   (READ_DATA, 32-bit, or READ_DATA_64), ends with END_IMAGE_TX and its
//!   status, the computer sends DONE and the boot ROM answers DONE_RESP, then
//!   starts the program ([`Sahara::upload`]).
//!
//! No USB here: the transport is the same one fastboot uses
//! ([`crate::fastboot::Transport`]), natively and over WebUSB. Everything the
//! device sends is checked (its command, its length field, every number in
//! it) and answered with a [`SaharaError`], never a panic. All numbers are
//! little-endian. The packet layouts follow what public sources agree on
//! (Linux `drivers/accel/qaic/sahara.c`, the `edl` tool); where they don't,
//! the choice is noted at the constant.
//!
//! The receiver treats the device's packets as a byte stream: it reads the
//! 8-byte header, then exactly the length the header names, so a packet split
//! over transfers or two packets in one transfer read the same as one packet
//! per transfer (what a boot ROM sends over USB).

use crate::fastboot::{FbError, Transport};
use std::fmt;

/// The boot ROM's USB ids in emergency download mode (Qualcomm HS-USB QDLoader 9008).
pub const EDL_VENDOR_ID: u16 = 0x05c6;
pub const EDL_PRODUCT_ID: u16 = 0x9008;

/// The protocol version the computer answers with. Version 2 is what the
/// SDM636 era boot ROMs report and what both public sources answer; a boot
/// ROM whose oldest version is newer than this is refused
/// ([`SaharaError::Unsupported`]).
pub const HOST_VERSION: u32 = 2;
/// The oldest version the computer says it speaks. Linux answers 2, `edl`
/// answers 1; the boot ROM checks it against its own oldest version, and
/// both work in practice. 1 is the more permissive and is what `edl` uses
/// against phones.
pub const HOST_VERSION_MIN: u32 = 1;

/// Command ids (the first word of every packet).
pub mod cmd {
    pub const HELLO: u32 = 0x01;
    pub const HELLO_RESP: u32 = 0x02;
    pub const READ_DATA: u32 = 0x03;
    pub const END_IMAGE_TX: u32 = 0x04;
    pub const DONE: u32 = 0x05;
    pub const DONE_RESP: u32 = 0x06;
    pub const RESET: u32 = 0x07;
    pub const RESET_RESP: u32 = 0x08;
    pub const MEMORY_DEBUG: u32 = 0x09;
    pub const MEMORY_READ: u32 = 0x0a;
    pub const CMD_READY: u32 = 0x0b;
    pub const SWITCH_MODE: u32 = 0x0c;
    pub const EXECUTE: u32 = 0x0d;
    pub const EXECUTE_RESP: u32 = 0x0e;
    pub const EXECUTE_DATA: u32 = 0x0f;
    pub const MEMORY_DEBUG_64: u32 = 0x10;
    pub const MEMORY_READ_64: u32 = 0x11;
    pub const READ_DATA_64: u32 = 0x12;
    pub const RESET_STATE_MACHINE: u32 = 0x13;
}

/// The packet header: command and length, 4 bytes each.
pub const HEADER: usize = 8;
/// The longest packet the device sends (HELLO).
pub const PACKET_MAX: usize = 0x30;
/// The most a command-mode answer may carry. The known answers are at most a
/// few dozen bytes (a key hash repeated a few times); anything larger is
/// refused before it is read.
pub const EXEC_DATA_MAX: u32 = 4096;
/// The longest single transfer [`Sahara::upload`] sends; a larger READ_DATA
/// is answered in pieces of this size (each a multiple of every USB packet
/// size, so the device sees one continuous stream).
pub const SEND_CHUNK: usize = 1 << 20;

/// The modes of HELLO and HELLO_RESP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Send an image (the program).
    ImageTxPending,
    ImageTxComplete,
    /// The device crashed and offers its memory (not used here).
    MemoryDebug,
    /// Read-only queries (EXECUTE).
    Command,
    Other(u32),
}

impl Mode {
    pub fn from_u32(v: u32) -> Self {
        match v {
            0 => Mode::ImageTxPending,
            1 => Mode::ImageTxComplete,
            2 => Mode::MemoryDebug,
            3 => Mode::Command,
            v => Mode::Other(v),
        }
    }

    pub fn as_u32(self) -> u32 {
        match self {
            Mode::ImageTxPending => 0,
            Mode::ImageTxComplete => 1,
            Mode::MemoryDebug => 2,
            Mode::Command => 3,
            Mode::Other(v) => v,
        }
    }
}

/// The client commands of command mode (EXECUTE). Only reads exist here; the
/// two that switch the boot ROM to an older download protocol (4, 5) are left
/// out on purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exec {
    /// 0x01: the chip's serial number, 4 bytes.
    SerialNumber,
    /// 0x02: the MSM hardware id, 8 bytes ([`HwId`]).
    MsmHwId,
    /// 0x03: the hash of the OEM's root certificate, as burned into the
    /// chip's fuses (or Qualcomm's test root on an unfused chip).
    OemPkHash,
    /// 0x06: debug data. Not read by [`Sahara::read_facts`].
    ReadDebugData,
    /// 0x07: the boot ROM's ("SBL") software version, 4 bytes. Public
    /// sources list this as 7, not 6 (6 is the debug data); see
    /// [`Sahara::read_facts`].
    SblVersion,
}

impl Exec {
    pub fn id(self) -> u32 {
        match self {
            Exec::SerialNumber => 0x01,
            Exec::MsmHwId => 0x02,
            Exec::OemPkHash => 0x03,
            Exec::ReadDebugData => 0x06,
            Exec::SblVersion => 0x07,
        }
    }
}

/// The NAK statuses of END_IMAGE_TX, the ones that help a person: the rest
/// are shown as their number. Numbers follow the status enum of the `edl`
/// tool (`sahara_defs.py`); its description table and the enum's trailing
/// comments are shifted by one (each describes the next constant, e.g.
/// `UNKNOWN_HOST_ERROR = 0x15  # Timeout while receiving data`), so the
/// names here come from the enum identifiers, not the descriptions.
pub const STATUS_NAMES: &[(u32, &str)] = &[
    (0x00, "success"),
    (0x01, "invalid command in this state"),
    (0x02, "protocol mismatch"),
    (0x03, "invalid target protocol version"),
    (0x04, "invalid host protocol version"),
    (0x05, "invalid packet size"),
    (0x06, "unexpected image id"),
    (0x0d, "error while sending READ_DATA"),
    (0x0e, "unsupported number of program headers"),
    (0x0f, "invalid program header size"),
    (0x12, "invalid destination address"),
    (0x14, "invalid ELF header"),
    (0x18, "invalid mode"),
    (0x1d, "command failed"),
    (0x1f, "unsupported command"),
    (0x21, "hash table authentication failed"),
    (0x22, "segment hash verification failed"),
    (0x23, "no hash table in the image"),
    (0x24, "target init failure"),
    (0x25, "image authentication failed (not signed for this chip)"),
    (0x26, "invalid image hash table size"),
    (0x27, "enumeration failure"),
    (0x28, "hardware bulk transfer error"),
];

pub fn status_name(status: u32) -> &'static str {
    STATUS_NAMES.iter().find(|(s, _)| *s == status).map_or("unknown status", |(_, n)| n)
}

/// The boot ROM's HELLO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hello {
    pub version: u32,
    /// The oldest version it speaks.
    pub version_min: u32,
    /// The longest command packet it takes.
    pub max_cmd_len: u32,
    pub mode: Mode,
}

/// One packet from the device, its fields checked for length.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Packet {
    Hello(Hello),
    /// READ_DATA (`wide` false) or READ_DATA_64 (`wide` true).
    ReadData {
        image_id: u64,
        offset: u64,
        len: u64,
        wide: bool,
    },
    EndImageTx {
        image_id: u32,
        status: u32,
    },
    DoneResp {
        status: u32,
    },
    ResetResp,
    CmdReady,
    ExecuteResp {
        client_cmd: u32,
        data_len: u32,
    },
    /// MEMORY_DEBUG (`wide` false) or its 64-bit form: the device crashed and
    /// offers a memory table. Recognised only to be refused by name.
    MemoryDebug {
        table: u64,
        len: u64,
        wide: bool,
    },
}

impl Packet {
    pub fn name(&self) -> &'static str {
        match self {
            Packet::Hello(_) => "HELLO",
            Packet::ReadData { wide: false, .. } => "READ_DATA",
            Packet::ReadData { wide: true, .. } => "READ_DATA_64",
            Packet::EndImageTx { .. } => "END_IMAGE_TX",
            Packet::DoneResp { .. } => "DONE_RESP",
            Packet::ResetResp => "RESET_RESP",
            Packet::CmdReady => "CMD_READY",
            Packet::ExecuteResp { .. } => "EXECUTE_RESP",
            Packet::MemoryDebug { .. } => "MEMORY_DEBUG",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaharaError {
    Transport(FbError),
    /// The device stopped sending in the middle of a packet or an answer.
    Short {
        want: usize,
        got: usize,
    },
    /// A length field that isn't the one its command has.
    BadLength {
        cmd: u32,
        len: u32,
    },
    /// A command id the protocol doesn't have (or that a device never sends).
    UnknownCommand(u32),
    /// A valid packet at the wrong time.
    Unexpected {
        want: &'static str,
        got: &'static str,
    },
    /// The device ended with END_IMAGE_TX and a non-zero status.
    Nak {
        status: u32,
        during: &'static str,
    },
    /// A READ_DATA beyond the image's end (or of zero bytes); nothing was sent.
    OutOfRange {
        offset: u64,
        len: u64,
        image_len: u64,
    },
    /// A READ_DATA for another image than the one being sent.
    OtherImage {
        first: u64,
        now: u64,
    },
    /// An EXECUTE_RESP for another command, or an answer too long or too short.
    BadAnswer(String),
    /// The device asks for more than a few times the image: it is looping.
    TooManyReads {
        served: u64,
    },
    /// A boot ROM whose oldest version is newer than this computer speaks.
    Unsupported {
        version: u32,
        version_min: u32,
    },
    /// Called in the wrong state (a mistake of the caller, not the device).
    State(&'static str),
}

impl fmt::Display for SaharaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaharaError::Transport(e) => write!(f, "{e}"),
            SaharaError::Short { want, got } => write!(f, "the device stopped after {got} of {want} bytes"),
            SaharaError::BadLength { cmd, len } => write!(f, "packet {cmd:#x} with length {len}, not its command's"),
            SaharaError::UnknownCommand(c) => write!(f, "unknown packet {c:#x}"),
            SaharaError::Unexpected { want, got } => write!(f, "the device sent {got} where {want} was due"),
            SaharaError::Nak { status, during } => write!(f, "the device refused {during}: {} ({status:#x})", status_name(*status)),
            SaharaError::OutOfRange { offset, len, image_len } => {
                write!(f, "the device asked for {len} bytes at {offset} of a {image_len}-byte image")
            }
            SaharaError::OtherImage { first, now } => write!(f, "the device asked for image {now} while sending image {first}"),
            SaharaError::BadAnswer(w) => write!(f, "bad answer: {w}"),
            SaharaError::TooManyReads { served } => write!(f, "the device keeps asking for data ({served} bytes sent)"),
            SaharaError::Unsupported { version, version_min } => {
                write!(
                    f,
                    "the boot ROM speaks Sahara {version_min}..{version}, this installer only {HOST_VERSION_MIN}..{HOST_VERSION}"
                )
            }
            SaharaError::State(w) => write!(f, "not now: {w}"),
        }
    }
}

impl std::error::Error for SaharaError {}

impl From<FbError> for SaharaError {
    fn from(e: FbError) -> Self {
        SaharaError::Transport(e)
    }
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    let mut w = [0u8; 4];
    w.copy_from_slice(&b[at..at + 4]);
    u32::from_le_bytes(w)
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    let mut w = [0u8; 8];
    w.copy_from_slice(&b[at..at + 8]);
    u64::from_le_bytes(w)
}

/// The length each command the device sends must have.
fn device_len(c: u32) -> Option<usize> {
    Some(match c {
        cmd::HELLO => 0x30,
        cmd::READ_DATA => 0x14,
        cmd::END_IMAGE_TX => 0x10,
        cmd::DONE_RESP => 0x0c,
        cmd::RESET_RESP | cmd::CMD_READY => 0x08,
        cmd::EXECUTE_RESP => 0x10,
        cmd::MEMORY_DEBUG => 0x10,
        cmd::MEMORY_DEBUG_64 => 0x18,
        cmd::READ_DATA_64 => 0x20,
        _ => return None,
    })
}

/// Parses one whole packet from the device: `buf` is exactly the packet.
pub fn parse_packet(buf: &[u8]) -> Result<Packet, SaharaError> {
    if buf.len() < HEADER {
        return Err(SaharaError::Short { want: HEADER, got: buf.len() });
    }
    let (c, len) = (u32_at(buf, 0), u32_at(buf, 4));
    let want = device_len(c).ok_or(SaharaError::UnknownCommand(c))?;
    if len as usize != want {
        return Err(SaharaError::BadLength { cmd: c, len });
    }
    if buf.len() != want {
        return Err(SaharaError::Short { want, got: buf.len() });
    }
    Ok(match c {
        cmd::HELLO => Packet::Hello(Hello {
            version: u32_at(buf, 8),
            version_min: u32_at(buf, 12),
            max_cmd_len: u32_at(buf, 16),
            mode: Mode::from_u32(u32_at(buf, 20)),
        }),
        cmd::READ_DATA => Packet::ReadData {
            image_id: u32_at(buf, 8).into(),
            offset: u32_at(buf, 12).into(),
            len: u32_at(buf, 16).into(),
            wide: false,
        },
        cmd::READ_DATA_64 => Packet::ReadData {
            image_id: u64_at(buf, 8),
            offset: u64_at(buf, 16),
            len: u64_at(buf, 24),
            wide: true,
        },
        cmd::END_IMAGE_TX => Packet::EndImageTx {
            image_id: u32_at(buf, 8),
            status: u32_at(buf, 12),
        },
        cmd::DONE_RESP => Packet::DoneResp { status: u32_at(buf, 8) },
        cmd::RESET_RESP => Packet::ResetResp,
        cmd::CMD_READY => Packet::CmdReady,
        cmd::EXECUTE_RESP => Packet::ExecuteResp {
            client_cmd: u32_at(buf, 8),
            data_len: u32_at(buf, 12),
        },
        cmd::MEMORY_DEBUG => Packet::MemoryDebug {
            table: u32_at(buf, 8).into(),
            len: u32_at(buf, 12).into(),
            wide: false,
        },
        _ => Packet::MemoryDebug {
            table: u64_at(buf, 8),
            len: u64_at(buf, 16),
            wide: true,
        },
    })
}

fn packet(c: u32, body: &[u32]) -> Vec<u8> {
    let len = HEADER + 4 * body.len();
    let mut v = Vec::with_capacity(len);
    v.extend_from_slice(&c.to_le_bytes());
    v.extend_from_slice(&(len as u32).to_le_bytes());
    for w in body {
        v.extend_from_slice(&w.to_le_bytes());
    }
    v
}

/// HELLO_RESP: version, oldest version, status 0, mode, six reserved zero
/// words (0x30 bytes, the length of HELLO).
pub fn hello_resp(mode: Mode) -> Vec<u8> {
    packet(cmd::HELLO_RESP, &[HOST_VERSION, HOST_VERSION_MIN, 0, mode.as_u32(), 0, 0, 0, 0, 0, 0])
}

pub fn execute(c: Exec) -> Vec<u8> {
    packet(cmd::EXECUTE, &[c.id()])
}

pub fn execute_data(c: Exec) -> Vec<u8> {
    packet(cmd::EXECUTE_DATA, &[c.id()])
}

pub fn switch_mode(mode: Mode) -> Vec<u8> {
    packet(cmd::SWITCH_MODE, &[mode.as_u32()])
}

pub fn done() -> Vec<u8> {
    packet(cmd::DONE, &[])
}

pub fn reset() -> Vec<u8> {
    packet(cmd::RESET, &[])
}

pub fn reset_state_machine() -> Vec<u8> {
    packet(cmd::RESET_STATE_MACHINE, &[])
}

/// The MSM hardware id (EXECUTE 0x02), split the way loader file names and
/// `edl` print it: `MMMMMMMM OOOO DDDD` in hex, the msm id in the upper 32
/// bits, then the OEM id and the model id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HwId {
    pub msm_id: u32,
    pub oem_id: u16,
    pub model_id: u16,
}

impl HwId {
    /// `edl` keeps only the low 24 bits of the msm id (it drops the top
    /// byte); this keeps all 32. For every id seen so far the top byte is 0,
    /// so both agree; what a non-zero top byte means (a revision?) is not
    /// known here.
    pub fn from_u64(v: u64) -> Self {
        HwId {
            msm_id: (v >> 32) as u32,
            oem_id: (v >> 16) as u16,
            model_id: v as u16,
        }
    }

    /// The chip, when it is one this installer knows.
    pub fn chip(&self) -> Option<&'static str> {
        KNOWN_CHIPS.iter().find(|(id, _)| *id == self.msm_id).map(|(_, n)| *n)
    }
}

/// The msm ids the installer recognises. SDM636 is `0x000cc0e1`, the msm part
/// of the hardware id as it appears in public loader file names
/// (`000cc0e1<oem><model>_<pk hash>…`). Only add an id seen on a real unit or
/// in two independent public sources.
pub const KNOWN_CHIPS: &[(u32, &str)] = &[(0x000c_c0e1, "SDM636")];

/// What the boot ROM reports in command mode, before anything is sent.
///
/// `serial` is unique to the unit: it is for the session (telling two units
/// apart) and never goes into a report, a log or anything shared. `Debug`
/// leaves it out. `pk_hash` is the same on every unit of a model (the OEM's
/// key), but it is still only shown to the person, never uploaded.
#[derive(Clone, PartialEq, Eq)]
pub struct EmergencyFacts {
    pub sahara_version: u32,
    pub sahara_version_min: u32,
    pub serial: u32,
    pub hw_id: u64,
    /// The raw answer to EXECUTE 0x03 (see [`EmergencyFacts::pk_hash`]).
    pub pk_hash_raw: Vec<u8>,
    /// `None` when the boot ROM refused the question.
    pub sbl_version: Option<u32>,
}

impl fmt::Debug for EmergencyFacts {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EmergencyFacts")
            .field("sahara_version", &self.sahara_version)
            .field("sahara_version_min", &self.sahara_version_min)
            .field("serial", &"(not shown)")
            .field("hw_id", &format_args!("{:016x}", self.hw_id))
            .field("pk_hash", &self.pk_hash_hex())
            .field("sbl_version", &self.sbl_version)
            .finish()
    }
}

impl EmergencyFacts {
    pub fn hw(&self) -> HwId {
        HwId::from_u64(self.hw_id)
    }

    pub fn chip(&self) -> Option<&'static str> {
        self.hw().chip()
    }

    /// The key hash. Boot ROMs answer one hash, or one hash repeated (seen
    /// as a 32-byte SHA-256 or a 48-byte SHA-384 several times over); a
    /// repeat is folded to one copy. Any other shape is kept as it came.
    pub fn pk_hash(&self) -> &[u8] {
        let raw = &self.pk_hash_raw;
        for p in [32, 48] {
            if raw.len() > p && raw.len().is_multiple_of(p) && raw.chunks(p).all(|c| c == &raw[..p]) {
                return &raw[..p];
            }
        }
        raw
    }

    pub fn pk_hash_hex(&self) -> String {
        self.pk_hash().iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// How the device ended an image transfer (DONE_RESP's status).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoneStatus {
    /// It runs the image now.
    Complete,
    /// It wants another image: a fresh HELLO follows ([`Sahara::wait_hello`]).
    Pending,
    Other(u32),
}

/// What [`Sahara::upload`] saw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upload {
    pub image_id: u64,
    pub reads: u32,
    /// The bytes sent, counting a piece asked for twice twice.
    pub bytes: u64,
    /// Whether the device used READ_DATA_64.
    pub wide: bool,
    pub done: DoneStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// A HELLO is waiting for its answer.
    Hello,
    Command,
    /// Nothing is due from the computer: an upload finished, a reset was sent.
    Idle,
}

/// One Sahara conversation with a boot ROM.
pub struct Sahara<T: Transport> {
    t: T,
    rx: Vec<u8>,
    hello: Hello,
    phase: Phase,
}

impl<T: Transport> Sahara<T> {
    /// Waits for the boot ROM's HELLO (it sends one as soon as the computer
    /// opens the device, or after SWITCH_MODE / RESET_STATE_MACHINE).
    pub async fn connect(t: T) -> Result<Self, SaharaError> {
        let mut s = Sahara {
            t,
            rx: Vec::new(),
            hello: Hello {
                version: 0,
                version_min: 0,
                max_cmd_len: 0,
                mode: Mode::Other(u32::MAX),
            },
            phase: Phase::Idle,
        };
        s.wait_hello().await?;
        Ok(s)
    }

    /// The last HELLO.
    pub fn hello(&self) -> &Hello {
        &self.hello
    }

    /// Reads the next HELLO. Bytes the device sent before it (an answer
    /// nobody waited for) make this fail rather than be skipped.
    pub async fn wait_hello(&mut self) -> Result<&Hello, SaharaError> {
        match self.next().await? {
            Packet::Hello(h) => {
                if h.version_min > HOST_VERSION || h.version < h.version_min {
                    return Err(SaharaError::Unsupported {
                        version: h.version,
                        version_min: h.version_min,
                    });
                }
                self.hello = h;
                self.phase = Phase::Hello;
                Ok(&self.hello)
            }
            p => Err(SaharaError::Unexpected { want: "HELLO", got: p.name() }),
        }
    }

    /// Reads into the buffer until it holds `n` bytes. A transport timeout
    /// in the middle of a packet is [`SaharaError::Short`].
    async fn fill(&mut self, n: usize) -> Result<(), SaharaError> {
        while self.rx.len() < n {
            match self.t.recv_max(n - self.rx.len()).await {
                Ok(b) if b.is_empty() => return Err(SaharaError::Short { want: n, got: self.rx.len() }),
                Ok(b) => self.rx.extend_from_slice(&b),
                Err(FbError::Timeout(_)) if !self.rx.is_empty() => return Err(SaharaError::Short { want: n, got: self.rx.len() }),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    fn take(&mut self, n: usize) -> Vec<u8> {
        let rest = self.rx.split_off(n);
        std::mem::replace(&mut self.rx, rest)
    }

    /// The next whole packet from the device.
    pub async fn next(&mut self) -> Result<Packet, SaharaError> {
        self.fill(HEADER).await?;
        let (c, len) = (u32_at(&self.rx, 0), u32_at(&self.rx, 4));
        let want = device_len(c).ok_or_else(|| {
            self.rx.clear();
            SaharaError::UnknownCommand(c)
        })?;
        if len as usize != want {
            self.rx.clear();
            return Err(SaharaError::BadLength { cmd: c, len });
        }
        self.fill(want).await?;
        let p = self.take(want);
        parse_packet(&p)
    }

    /// Answers the HELLO with command mode; the boot ROM says CMD_READY.
    pub async fn enter_command_mode(&mut self) -> Result<(), SaharaError> {
        if self.phase != Phase::Hello {
            return Err(SaharaError::State("command mode is chosen in the answer to a HELLO"));
        }
        self.phase = Phase::Idle;
        self.t.send(&hello_resp(Mode::Command)).await?;
        match self.next().await? {
            Packet::CmdReady => {
                self.phase = Phase::Command;
                Ok(())
            }
            Packet::EndImageTx { status, .. } => Err(SaharaError::Nak {
                status,
                during: "command mode",
            }),
            p => Err(SaharaError::Unexpected {
                want: "CMD_READY",
                got: p.name(),
            }),
        }
    }

    /// One command-mode query: EXECUTE, its EXECUTE_RESP (the answer's
    /// length), EXECUTE_DATA, then the answer's bytes.
    pub async fn execute(&mut self, c: Exec) -> Result<Vec<u8>, SaharaError> {
        if self.phase != Phase::Command {
            return Err(SaharaError::State("EXECUTE needs command mode"));
        }
        self.t.send(&execute(c)).await?;
        let n = match self.next().await? {
            Packet::ExecuteResp { client_cmd, data_len } => {
                if client_cmd != c.id() {
                    return Err(SaharaError::BadAnswer(format!("EXECUTE_RESP for {client_cmd:#x}, asked {:#x}", c.id())));
                }
                if data_len > EXEC_DATA_MAX {
                    return Err(SaharaError::BadAnswer(format!("an answer of {data_len} bytes to {:#x}", c.id())));
                }
                data_len as usize
            }
            Packet::EndImageTx { status, .. } => {
                return Err(SaharaError::Nak {
                    status,
                    during: "a command-mode query",
                });
            }
            p => {
                return Err(SaharaError::Unexpected {
                    want: "EXECUTE_RESP",
                    got: p.name(),
                });
            }
        };
        self.t.send(&execute_data(c)).await?;
        self.fill(n).await?;
        Ok(self.take(n))
    }

    async fn exec_u32(&mut self, c: Exec) -> Result<u32, SaharaError> {
        let a = self.execute(c).await?;
        if a.len() < 4 {
            return Err(SaharaError::BadAnswer(format!("{} bytes to {:#x}, want 4", a.len(), c.id())));
        }
        Ok(u32_at(&a, 0))
    }

    /// Reads serial, hardware id, key hash and, last, the SBL version, in
    /// command mode. The SBL version (EXECUTE 0x07 in the public sources; the
    /// task that asked for it named 6, which those sources call debug data)
    /// is read last because some boot ROMs refuse it, and whether one still
    /// takes commands after refusing is not known: a refusal gives `None`.
    pub async fn read_facts(&mut self) -> Result<EmergencyFacts, SaharaError> {
        let serial = self.exec_u32(Exec::SerialNumber).await?;
        let hw = self.execute(Exec::MsmHwId).await?;
        if hw.len() < 8 {
            return Err(SaharaError::BadAnswer(format!("a hardware id of {} bytes, want 8", hw.len())));
        }
        let hw_id = u64_at(&hw, 0);
        let pk_hash_raw = self.execute(Exec::OemPkHash).await?;
        if pk_hash_raw.is_empty() {
            return Err(SaharaError::BadAnswer("an empty key hash".into()));
        }
        let sbl_version = match self.exec_u32(Exec::SblVersion).await {
            Ok(v) => Some(v),
            Err(SaharaError::Nak { .. }) => None,
            Err(e) => return Err(e),
        };
        Ok(EmergencyFacts {
            sahara_version: self.hello.version,
            sahara_version_min: self.hello.version_min,
            serial,
            hw_id,
            pk_hash_raw,
            sbl_version,
        })
    }

    /// Leaves command mode: SWITCH_MODE, then the boot ROM's fresh HELLO.
    /// Which mode to name is not settled in public sources: `edl` names
    /// command mode (3) and then answers the new HELLO with image transfer;
    /// either way the answer to the new HELLO picks the mode.
    pub async fn switch_mode(&mut self, mode: Mode) -> Result<&Hello, SaharaError> {
        if self.phase != Phase::Command {
            return Err(SaharaError::State("SWITCH_MODE needs command mode"));
        }
        self.phase = Phase::Idle;
        self.t.send(&switch_mode(mode)).await?;
        self.wait_hello().await
    }

    /// Answers the HELLO with image transfer and serves `image`: every
    /// READ_DATA / READ_DATA_64 from its bytes, then END_IMAGE_TX, DONE and
    /// DONE_RESP. A read beyond the image (or of zero bytes) ends it with
    /// [`SaharaError::OutOfRange`] and nothing sent for it; the device then
    /// waits, and the caller resets it ([`Sahara::reset`]). Whether the
    /// device needs a zero-length packet after a piece that is a whole
    /// number of USB packets is not known; none is sent (the device knows
    /// how much it asked for, and the public sources send none either).
    pub async fn upload(&mut self, image: &[u8]) -> Result<Upload, SaharaError> {
        if self.phase != Phase::Hello {
            return Err(SaharaError::State("an image is offered in the answer to a HELLO"));
        }
        self.phase = Phase::Idle;
        let image_len = image.len() as u64;
        let cap = image_len.saturating_mul(4).saturating_add(SEND_CHUNK as u64);
        self.t.send(&hello_resp(Mode::ImageTxPending)).await?;
        let mut up = Upload {
            image_id: 0,
            reads: 0,
            bytes: 0,
            wide: false,
            done: DoneStatus::Other(u32::MAX),
        };
        loop {
            match self.next().await? {
                Packet::ReadData { image_id, offset, len, wide } => {
                    if up.reads == 0 {
                        up.image_id = image_id;
                        up.wide = wide;
                    } else if image_id != up.image_id {
                        return Err(SaharaError::OtherImage {
                            first: up.image_id,
                            now: image_id,
                        });
                    }
                    let end = offset.checked_add(len).filter(|&e| len > 0 && e <= image_len);
                    let Some(end) = end else {
                        return Err(SaharaError::OutOfRange { offset, len, image_len });
                    };
                    if up.bytes.saturating_add(len) > cap {
                        return Err(SaharaError::TooManyReads { served: up.bytes });
                    }
                    // Both fit in usize: end <= image.len().
                    let (a, b) = (offset as usize, end as usize);
                    for piece in image[a..b].chunks(SEND_CHUNK) {
                        self.t.send(piece).await?;
                    }
                    up.reads += 1;
                    up.bytes += len;
                }
                Packet::EndImageTx { status: 0, .. } => break,
                Packet::EndImageTx { status, .. } => return Err(SaharaError::Nak { status, during: "the image" }),
                p => {
                    return Err(SaharaError::Unexpected {
                        want: "READ_DATA or END_IMAGE_TX",
                        got: p.name(),
                    });
                }
            }
        }
        self.t.send(&done()).await?;
        up.done = match self.next().await? {
            Packet::DoneResp { status: 1 } => DoneStatus::Complete,
            Packet::DoneResp { status: 0 } => DoneStatus::Pending,
            Packet::DoneResp { status } => DoneStatus::Other(status),
            Packet::EndImageTx { status, .. } => return Err(SaharaError::Nak { status, during: "DONE" }),
            p => {
                return Err(SaharaError::Unexpected {
                    want: "DONE_RESP",
                    got: p.name(),
                });
            }
        };
        Ok(up)
    }

    /// RESET: the boot ROM answers RESET_RESP and restarts the device.
    pub async fn reset(&mut self) -> Result<(), SaharaError> {
        self.phase = Phase::Idle;
        self.rx.clear();
        self.t.send(&reset()).await?;
        match self.next().await? {
            Packet::ResetResp => Ok(()),
            Packet::EndImageTx { status, .. } => Err(SaharaError::Nak { status, during: "RESET" }),
            p => Err(SaharaError::Unexpected {
                want: "RESET_RESP",
                got: p.name(),
            }),
        }
    }

    /// RESET_STATE_MACHINE: the boot ROM starts the protocol over without
    /// restarting the device. No public source says it answers anything but
    /// a fresh HELLO, so this waits for none: follow with
    /// [`Sahara::wait_hello`].
    pub async fn reset_state_machine(&mut self) -> Result<(), SaharaError> {
        self.phase = Phase::Idle;
        self.rx.clear();
        self.t.send(&reset_state_machine()).await?;
        Ok(())
    }

    pub fn into_transport(self) -> T {
        self.t
    }

    pub fn transport(&self) -> &T {
        &self.t
    }
}

/// Reads what the boot ROM reports and leaves command mode again (SWITCH_MODE
/// to command mode, as `edl` does, then the fresh HELLO): the device is back
/// at a HELLO, nothing was sent to it but queries.
pub async fn emergency_info<T: Transport>(s: &mut Sahara<T>) -> Result<EmergencyFacts, SaharaError> {
    s.enter_command_mode().await?;
    let f = s.read_facts().await?;
    s.switch_mode(Mode::Command).await?;
    Ok(f)
}

/// What `emergency-info` shows of [`EmergencyFacts`]: the numbers as hex
/// strings, the way the USB ids of a facts snapshot are written. The serial
/// is in it only when asked for ([`InfoReport::new`]'s `with_serial`): it is
/// unique to the unit, so nothing shown by default carries it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct InfoReport {
    /// The USB ids the device was found by, `05c6` and `9008`.
    pub vendor_id: String,
    pub product_id: String,
    pub sahara_version: u32,
    pub sahara_version_min: u32,
    /// The MSM hardware id, all 16 hex digits.
    pub hw_id: String,
    pub msm_id: String,
    pub oem_id: String,
    pub model_id: String,
    /// The chip's name, when [`KNOWN_CHIPS`] has its msm id.
    pub chip: Option<String>,
    /// The OEM's key hash, folded ([`EmergencyFacts::pk_hash`]).
    pub oem_pk_hash: String,
    /// `None` when the boot ROM refused the question.
    pub sbl_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
}

impl InfoReport {
    pub fn new(f: &EmergencyFacts, vendor_id: u16, product_id: u16, with_serial: bool) -> Self {
        let hw = f.hw();
        InfoReport {
            vendor_id: format!("{vendor_id:04x}"),
            product_id: format!("{product_id:04x}"),
            sahara_version: f.sahara_version,
            sahara_version_min: f.sahara_version_min,
            hw_id: format!("{:016x}", f.hw_id),
            msm_id: format!("{:08x}", hw.msm_id),
            oem_id: format!("{:04x}", hw.oem_id),
            model_id: format!("{:04x}", hw.model_id),
            chip: hw.chip().map(str::to_string),
            oem_pk_hash: f.pk_hash_hex(),
            sbl_version: f.sbl_version.map(|v| format!("{v:08x}")),
            serial: with_serial.then(|| format!("{:08x}", f.serial)),
        }
    }

    /// The text form, one fact a line.
    pub fn render(&self) -> String {
        let mut o = format!("emergency download device {}:{}\n", self.vendor_id, self.product_id);
        o += &format!("sahara: version {} (oldest {})\n", self.sahara_version, self.sahara_version_min);
        let ids = format!("msm id {}, oem id {}, model id {}", self.msm_id, self.oem_id, self.model_id);
        match &self.chip {
            Some(c) => o += &format!("chip: {c} ({ids})\n"),
            None => o += &format!("chip: not one this installer knows ({ids})\n"),
        }
        o += &format!("oem pk hash: {}\n", self.oem_pk_hash);
        match &self.sbl_version {
            Some(v) => o += &format!("sbl version: {v}\n"),
            None => o += "sbl version: not reported (the boot ROM refused the question)\n",
        }
        if let Some(s) = &self.serial {
            o += &format!("serial: {s}\n");
        }
        o
    }
}
