// SPDX-License-Identifier: GPL-2.0-or-later
//! The fastboot protocol: the reply parser, the transport an edge supplies,
//! the framings of FR-011 and a session that sends `getvar` and `reboot`. No
//! USB and no clocks here: the transport bounds every wait and answers
//! [`FbError::Timeout`] when one runs out.
//!
//! One more pair exists only inside the crate: `download` + `boot` of a boot
//! image, for [`crate::ram_backup`] alone (the RAM environment of the
//! backup).
//!
//! The writes of an install: [`Session::flash_raw`] (one `download` of a raw
//! image streamed from an [`AsyncReadAt`], then `flash:<partition>`),
//! [`Session::oem`] for the verbs a description allows and
//! [`Session::reboot_bootloader`]. Each is checked against [`WriteRules`]
//! before anything is sent. `erase` doesn't exist. `flash:partition` (any name
//! that starts with `partition`) is never sent: both Note Air boot loaders
//! rewrite the partition table from it after erasing the WHOLE eMMC.
//!
//! A transport error or a malformed reply on any command (reads and `reboot`
//! too) ends the session: nothing more is sent on it ([`FbError::Ended`]).

use crate::table::AsyncReadAt;
use std::fmt;

/// Fastboot replies are at most 64 bytes in the original protocol; newer
/// boot loaders send up to 256. Read with room for either.
pub const REPLY_MAX: usize = 256;

/// The longest command the protocol takes.
pub const COMMAND_MAX: usize = 64;

/// The fastboot USB interface: vendor-specific class, subclass 0x42, protocol 0x03.
pub const FB_CLASS: u8 = 0xff;
pub const FB_SUBCLASS: u8 = 0x42;
pub const FB_PROTOCOL: u8 = 0x03;

/// The variables with a parameter that a read may name: `partition-size:boot`,
/// and a slot's flags (`slot-successful:a`).
pub const PARAM_VARIABLES: &[&str] = &[
    "partition-size",
    "partition-type",
    "has-slot",
    "slot-successful",
    "slot-unbootable",
    "slot-retry-count",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Okay(String),
    Fail(String),
    Info(String),
    Text(String),
    Data(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FbError {
    /// The transport failed (the device went away, a stall, ...).
    Usb(String),
    /// A bounded wait ran out; the transport has cancelled what was pending,
    /// and the session is over.
    Timeout(String),
    /// The device answered FAIL where only OKAY makes sense.
    Fail(String),
    Malformed(Vec<u8>),
    /// Refused before anything was sent: not a plain read.
    Refused(String),
    /// The warm-up didn't give the expected answers: the mode wasn't freshly
    /// started (a departure's fresh-boot condition), or the answers stopped
    /// agreeing during the session.
    NotFresh(String),
    /// With plain framing, the device's answers don't agree with each other,
    /// and no description says how else to talk to it.
    Disagree(String),
    /// A write refused before it was sent ([`WriteRules`], a size, the name).
    RefusedWrite(String),
    /// An earlier failure ended the session: the boot loader may be in the
    /// middle of a download, so nothing more is sent on it.
    Ended(String),
}

impl fmt::Display for FbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FbError::Usb(e) => write!(f, "usb: {e}"),
            FbError::Timeout(e) => write!(f, "the device didn't answer in time ({e})"),
            FbError::Fail(e) => write!(f, "the device said FAIL: {e}"),
            FbError::Malformed(b) => write!(f, "malformed reply: {b:02x?}"),
            FbError::Refused(v) => write!(f, "refused before sending: {v:?} is not a plain read"),
            FbError::NotFresh(why) => write!(f, "not a fresh start of the mode ({why})"),
            FbError::Disagree(why) => write!(f, "its answers don't agree ({why}), and no device description says how to talk to it"),
            FbError::RefusedWrite(why) => write!(f, "refused before sending: {why}"),
            FbError::Ended(why) => write!(f, "the session ended earlier ({why}); nothing more is sent on it"),
        }
    }
}

impl std::error::Error for FbError {}

pub fn parse_reply(buf: &[u8]) -> Result<Reply, FbError> {
    if buf.len() < 4 || buf.len() > REPLY_MAX {
        return Err(FbError::Malformed(buf.to_vec()));
    }
    let (tag, rest) = buf.split_at(4);
    // Some boot loaders end the reply with a NUL (the Note Air after DATA).
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    let text = String::from_utf8_lossy(&rest[..end]).into_owned();
    match tag {
        b"OKAY" => Ok(Reply::Okay(text)),
        b"FAIL" => Ok(Reply::Fail(text)),
        b"INFO" => Ok(Reply::Info(text)),
        b"TEXT" => Ok(Reply::Text(text)),
        b"DATA" => u32::from_str_radix(&text, 16).map(Reply::Data).map_err(|_| FbError::Malformed(buf.to_vec())),
        _ => Err(FbError::Malformed(buf.to_vec())),
    }
}

/// One fastboot exchange medium: a bulk OUT write and a bulk IN read. Each
/// call is bounded by the transport: when its wait runs out it cancels the
/// transfer and returns [`FbError::Timeout`].
pub trait Transport {
    fn send(&mut self, data: &[u8]) -> impl Future<Output = Result<(), FbError>>;
    fn recv(&mut self) -> impl Future<Output = Result<Vec<u8>, FbError>>;
    /// One bulk IN transfer of up to `max` bytes (rounded up to the endpoint's
    /// packet size by the edge), for protocols that stream data
    /// ([`crate::glassine_read`]). It returns what one transfer delivered, which
    /// may be less than `max`. A transport that only carries short replies
    /// keeps the default: one [`Transport::recv`].
    fn recv_max(&mut self, max: usize) -> impl Future<Output = Result<Vec<u8>, FbError>> {
        let _ = max;
        self.recv()
    }
    /// Bounds the waits of the transfers that follow by `wait` instead of the
    /// transport's own bound; `None` restores that. For a protocol that
    /// polls (Firehose's first `configure`, [`crate::firehose`]): a timeout
    /// must leave the transport usable. A transport where it doesn't (the
    /// browser's closes on its first timeout) keeps this default, which
    /// changes nothing.
    fn set_short_wait(&mut self, wait: Option<std::time::Duration>) {
        let _ = wait;
    }
}

impl<T: Transport> Transport for &mut T {
    fn send(&mut self, data: &[u8]) -> impl Future<Output = Result<(), FbError>> {
        (**self).send(data)
    }
    fn recv(&mut self) -> impl Future<Output = Result<Vec<u8>, FbError>> {
        (**self).recv()
    }
    fn recv_max(&mut self, max: usize) -> impl Future<Output = Result<Vec<u8>, FbError>> {
        (**self).recv_max(max)
    }
    fn set_short_wait(&mut self, wait: Option<std::time::Duration>) {
        (**self).set_short_wait(wait)
    }
}

/// How commands are framed on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    /// Every command as it is (the protocol as specified).
    Plain,
    /// After a warm-up, every transfer exactly `len` bytes, commands NUL-padded
    /// (the `equal-length-transfers` departure).
    EqualLength { len: usize },
}

impl Framing {
    pub fn name(&self) -> String {
        match self {
            Framing::Plain => "plain".into(),
            Framing::EqualLength { len } => format!("equal-length {len}"),
        }
    }
}

/// Checks that `name` is one variable to read: a plain name, or one of
/// [`PARAM_VARIABLES`] with one plain parameter. Never `all`, never a name
/// that reads a value unique to the unit.
pub fn readable_variable(name: &str) -> Result<(), FbError> {
    let plain = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b));
    let ok = match name.split_once(':') {
        None => plain(name),
        Some((v, p)) => PARAM_VARIABLES.contains(&v) && plain(p),
    };
    if !ok || name == "all" || crate::vocab::looks_unit_unique(name) || "getvar:".len() + name.len() > COMMAND_MAX {
        return Err(FbError::Refused(name.to_string()));
    }
    Ok(())
}

/// The first exchange of every session (FR-012): one short, plain
/// `getvar:product`. Safe under every framing the format knows, since the
/// equal-length warm-up starts with exactly this transfer. Returns the reply
/// to it (after any INFO lines).
pub async fn opening<T: Transport>(t: &mut T) -> Result<Reply, FbError> {
    t.send(b"getvar:product").await?;
    loop {
        match parse_reply(&t.recv().await?)? {
            Reply::Info(_) | Reply::Text(_) => continue,
            r => return Ok(r),
        }
    }
}

/// One fastboot connection after the opening, with its framing: reads,
/// `reboot`, and the writes [`WriteRules`] allow.
pub struct Session<T: Transport> {
    t: T,
    framing: Framing,
    /// The opening's answer: the product, or `None` when it answered FAIL.
    product: Option<String>,
    /// Why the session ended, once a failure left the boot loader in an
    /// unknown state (a cut download, a transfer that failed).
    ended: Option<String>,
}

impl<T: Transport> Session<T> {
    /// Goes on after [`opening`] answered `product` (`None` for FAIL). Plain
    /// framing needs nothing more. Equal-length framing sends the warm-up's
    /// dummy of `len` bytes (answered FAIL); then, in either framing, a
    /// `getvar:product` must answer what the opening did, or the session is
    /// refused with [`FbError::NotFresh`].
    pub async fn start(mut t: T, framing: Framing, product: Option<String>) -> Result<Self, FbError> {
        if let Framing::EqualLength { len } = framing {
            if product.is_none() {
                return Err(FbError::NotFresh("the opening got no product, so the warm-up has nothing to check".into()));
            }
            if len < COMMAND_MAX || len % 64 == 0 {
                return Err(FbError::Refused(format!("equal length {len}")));
            }
            t.send(&vec![b'x'; len]).await?;
            match parse_reply(&t.recv().await?) {
                Ok(Reply::Fail(_)) => {}
                r => return Err(FbError::NotFresh(format!("the warm-up's dummy got {r:?}, not FAIL"))),
            }
        }
        let mut s = Session {
            t,
            framing,
            product,
            ended: None,
        };
        s.check_product("the warm-up's check").await?;
        Ok(s)
    }

    pub fn framing(&self) -> Framing {
        self.framing
    }

    pub fn product(&self) -> Option<&str> {
        self.product.as_deref()
    }

    /// Reads `product` again and requires the opening's answer: a session
    /// whose framing has slipped gives something else, never a value that
    /// gets recorded.
    pub async fn check_product(&mut self, when: &str) -> Result<(), FbError> {
        match self.getvar("product").await {
            Ok(p) if p == self.product => Ok(()),
            Err(e) => Err(e),
            // After a warm-up, a different answer means the device wasn't in
            // the state the warm-up needs. With plain framing there was no
            // warm-up to judge: its answers just don't agree.
            Ok(p) => {
                let why = format!("{when}: product read {p:?}, the opening said {:?}", self.product);
                Err(match self.framing {
                    Framing::EqualLength { .. } => FbError::NotFresh(why),
                    Framing::Plain => FbError::Disagree(why),
                })
            }
        }
    }

    /// Why the session ended, if it did: nothing more is sent on it.
    pub fn ended(&self) -> Option<&str> {
        self.ended.as_deref()
    }

    /// Ends the session with `e` and returns it.
    fn end(&mut self, e: FbError) -> FbError {
        if self.ended.is_none() && !matches!(e, FbError::Ended(_)) {
            self.ended = Some(e.to_string());
        }
        e
    }

    /// One OUT transfer; a failed one ends the session.
    async fn send(&mut self, data: &[u8]) -> Result<(), FbError> {
        if let Some(why) = &self.ended {
            return Err(FbError::Ended(why.clone()));
        }
        match self.t.send(data).await {
            Ok(()) => Ok(()),
            Err(e) => Err(self.end(e)),
        }
    }

    /// One IN transfer, parsed; a failed or malformed one ends the session.
    async fn reply(&mut self) -> Result<Reply, FbError> {
        if let Some(why) = &self.ended {
            return Err(FbError::Ended(why.clone()));
        }
        match self.t.recv().await.and_then(|b| parse_reply(&b)) {
            Ok(r) => Ok(r),
            Err(e) => Err(self.end(e)),
        }
    }

    async fn command(&mut self, cmd: &str) -> Result<(), FbError> {
        match self.framing {
            Framing::Plain => self.send(cmd.as_bytes()).await,
            Framing::EqualLength { len } => {
                let mut b = vec![0u8; len];
                b[..cmd.len()].copy_from_slice(cmd.as_bytes());
                self.send(&b).await
            }
        }
    }

    /// Reads replies up to OKAY (`Some`) or FAIL (`None`), skipping INFO/TEXT.
    async fn finish(&mut self) -> Result<Option<String>, FbError> {
        loop {
            match self.reply().await? {
                Reply::Okay(v) => return Ok(Some(v)),
                Reply::Fail(_) => return Ok(None),
                Reply::Info(_) | Reply::Text(_) => {}
                Reply::Data(_) => return Err(self.end(FbError::Malformed(b"DATA where OKAY or FAIL is due".to_vec()))),
            }
        }
    }

    /// Like [`Session::finish`], with FAIL as [`FbError::Fail`] carrying what
    /// the boot loader said.
    async fn done(&mut self, what: &str) -> Result<(), FbError> {
        loop {
            match self.reply().await? {
                Reply::Okay(_) => return Ok(()),
                Reply::Fail(w) => return Err(FbError::Fail(format!("{what}: {w}"))),
                Reply::Info(_) | Reply::Text(_) => {}
                Reply::Data(_) => return Err(self.end(FbError::Malformed(b"DATA where OKAY or FAIL is due".to_vec()))),
            }
        }
    }

    /// `getvar:<name>`: the value, or `None` when the boot loader answered
    /// FAIL. Anything but one readable variable is refused before sending.
    pub async fn getvar(&mut self, name: &str) -> Result<Option<String>, FbError> {
        readable_variable(name)?;
        self.command(&format!("getvar:{name}")).await?;
        self.finish().await
    }

    /// `reboot`: leaves the mode and starts the device normally.
    pub async fn reboot(&mut self) -> Result<(), FbError> {
        self.command("reboot").await?;
        match self.finish().await? {
            Some(_) => Ok(()),
            None => Err(FbError::Fail("reboot".into())),
        }
    }

    /// `reboot-bootloader`: restarts into this fastboot mode, freshly started.
    pub async fn reboot_bootloader(&mut self) -> Result<(), FbError> {
        self.command("reboot-bootloader").await?;
        self.done("reboot-bootloader").await
    }

    /// `oem <verb>`: only a verb on `rules.oem` (the description's list), as
    /// it stands there: no arguments. Refused before sending otherwise.
    pub async fn oem(&mut self, rules: &WriteRules, verb: &str) -> Result<(), FbError> {
        if !plain_name(verb) || !rules.oem.iter().any(|v| v == verb) || "oem ".len() + verb.len() > COMMAND_MAX {
            return Err(FbError::RefusedWrite(format!("oem {verb:?} is not a verb the device description allows")));
        }
        rules.check_framing(self.framing)?;
        self.command(&format!("oem {verb}")).await?;
        self.done(&format!("oem {verb}")).await
    }

    /// Writes `len` bytes of `src` (from offset 0) to partition `part`: one
    /// `download`, then `flash:<part>`. The image is streamed in transfers of
    /// the framing's length (1 MiB with plain framing), never held whole;
    /// under equal-length framing the last transfer is NUL-padded, and those
    /// zeros land in the partition after the image (the boot loader writes
    /// the whole download).
    ///
    /// Before anything is sent: the name ([`WriteRules::check_partition`]),
    /// the framing, `len` > 0 and the image's first bytes (an image the boot
    /// loader would take for a sparse, meta or UBI image is not raw). Then,
    /// only reads: `partition-size:<part>` and `max-download-size`; the
    /// download's size (padded) must fit both. After the download, a
    /// `getvar:product` must still answer the opening's product before
    /// `flash:` goes out, so a download that ended early (a boot loader that
    /// said OKAY before it had all the data) never reaches the partition.
    /// The boot loader keeps its download across a `getvar` (abl
    /// `FastbootCmds.c`: only `download` sets the flash size); untried on the
    /// device.
    ///
    /// Any failure after the first byte of the download ends the session.
    pub async fn flash_raw<R: AsyncReadAt + ?Sized>(&mut self, rules: &WriteRules, part: &str, src: &mut R, len: u64) -> Result<(), WriteError> {
        rules.check_partition(part).map_err(WriteError::Refused)?;
        rules.check_framing(self.framing).map_err(WriteError::Refused)?;
        if let Some(why) = &self.ended {
            return Err(WriteError::Refused(FbError::Ended(why.clone())));
        }
        if len == 0 {
            return Err(WriteError::Refused(FbError::RefusedWrite(format!("an empty image for {part}"))));
        }
        let mut head = [0u8; 4];
        let n = len.min(4) as usize;
        src.read_at(0, &mut head[..n])
            .await
            .map_err(|e| WriteError::Refused(FbError::RefusedWrite(format!("the image for {part}: {e}"))))?;
        if let Some(kind) = not_raw(&head[..n]) {
            return Err(WriteError::Refused(FbError::RefusedWrite(format!(
                "the image for {part} starts like a {kind} image, which the boot loader would not write as it is"
            ))));
        }
        let size = download_size(len, self.framing);

        // Only reads until here.
        let psize = self.number(&format!("partition-size:{part}")).await.map_err(WriteError::Refused)?;
        let max = self.number("max-download-size").await.map_err(WriteError::Refused)?;
        let refuse = |why: String| Err(WriteError::Refused(FbError::RefusedWrite(why)));
        match (psize, max) {
            (None, _) => return refuse(format!("the boot loader doesn't say how large {part} is (partition-size)")),
            (_, None) => return refuse("the boot loader doesn't say how much it takes (max-download-size)".into()),
            (Some(p), _) if size > p => {
                return refuse(format!(
                    "{part} holds {p} bytes; the image ({len} bytes) is sent as {size}, and all of it would be written"
                ));
            }
            (_, Some(m)) if size > m => return refuse(format!("the image is sent as {size} bytes, more than the boot loader takes ({m})")),
            _ => {}
        }
        let Ok(size32) = u32::try_from(size) else {
            return refuse(format!("a download of {size} bytes"));
        };

        self.download(src, len, size32).await.map_err(WriteError::BeforeFlash)?;
        if let Err(e) = self.check_product("after the download").await {
            return Err(WriteError::BeforeFlash(self.end(e)));
        }
        self.command(&format!("flash:{part}")).await.map_err(WriteError::Flash)?;
        match self.done(&format!("flash:{part}")).await {
            Ok(()) => Ok(()),
            // No answer: the write may still be going on. Nothing more is sent.
            Err(e) => Err(WriteError::Flash(self.end(e))),
        }
    }

    /// A number a variable holds (hex with `0x`, else decimal), `None` when
    /// the boot loader answers FAIL or something that isn't a number.
    async fn number(&mut self, var: &str) -> Result<Option<u64>, FbError> {
        Ok(self.getvar(var).await?.as_deref().and_then(parse_number))
    }

    /// `download:<size>` and the data: `len` bytes of `src`, NUL-padded to
    /// `size`. A failure ends the session.
    async fn download<R: AsyncReadAt + ?Sized>(&mut self, src: &mut R, len: u64, size: u32) -> Result<(), FbError> {
        let chunk = match self.framing {
            Framing::Plain => PLAIN_CHUNK,
            Framing::EqualLength { len } => len,
        };
        self.command(&format!("download:{size:08x}")).await?;
        loop {
            let e = match self.reply().await? {
                Reply::Data(n) if n == size => break,
                Reply::Data(n) => FbError::Fail(format!("download: the boot loader takes {n} bytes, not {size}")),
                Reply::Fail(w) => FbError::Fail(format!("download: {w}")),
                Reply::Okay(_) => FbError::Malformed(b"OKAY before the download's data".to_vec()),
                Reply::Info(_) | Reply::Text(_) => continue,
            };
            return Err(self.end(e));
        }
        let mut buf = vec![0u8; chunk];
        let (mut off, size) = (0u64, size as u64);
        while off < size {
            let n = (chunk as u64).min(size - off) as usize;
            let have = len.saturating_sub(off).min(n as u64) as usize;
            if let Err(e) = src.read_at(off, &mut buf[..have]).await {
                // The boot loader waits for the rest of the data: the session is over.
                return Err(self.end(FbError::Usb(format!("reading the image at {off}: {e}"))));
            }
            buf[have..n].fill(0);
            self.send(&buf[..n]).await?;
            off += n as u64;
        }
        match self.done("download: the boot loader refused the data").await {
            Ok(()) => Ok(()),
            Err(e) => Err(self.end(e)),
        }
    }

    /// `download` of `image`, then `boot`: the boot loader starts the image
    /// from memory. Only [`crate::ram_backup`] calls this; no public session
    /// sends either command. Under equal-length framing the image is
    /// NUL-padded to a whole number of transfers, each one exactly the
    /// framing's length, and the download announces the padded size.
    pub(crate) async fn download_and_boot(&mut self, image: &[u8]) -> Result<(), FbError> {
        let (chunk, size) = match self.framing {
            Framing::Plain => (1 << 20, image.len()),
            Framing::EqualLength { len } => (len, image.len().div_ceil(len) * len),
        };
        let size32 = u32::try_from(size).map_err(|_| FbError::Refused(format!("a download of {size} bytes")))?;
        self.command(&format!("download:{size32:08x}")).await?;
        loop {
            match self.reply().await? {
                Reply::Data(n) if n == size32 => break,
                Reply::Data(n) => return Err(FbError::Fail(format!("download: the boot loader takes {n} bytes, not {size32}"))),
                Reply::Fail(w) => return Err(FbError::Fail(format!("download: {w}"))),
                Reply::Okay(_) => return Err(FbError::Malformed(b"OKAY before the download's data".to_vec())),
                Reply::Info(_) | Reply::Text(_) => {}
            }
        }
        let mut buf = vec![0u8; chunk];
        let mut off = 0;
        while off < size {
            let n = chunk.min(size - off);
            let have = image.len().saturating_sub(off).min(n);
            buf[..have].copy_from_slice(&image[off..off + have]);
            buf[have..n].fill(0);
            self.send(&buf[..n]).await?;
            off += n;
        }
        if self.finish().await?.is_none() {
            return Err(FbError::Fail("download: the boot loader refused the data".into()));
        }
        self.command("boot").await?;
        match self.finish().await? {
            Some(_) => Ok(()),
            None => Err(FbError::Fail("boot".into())),
        }
    }

    pub fn into_transport(self) -> T {
        self.t
    }

    pub fn transport(&self) -> &T {
        &self.t
    }
}

/// Transfers of a download under plain framing.
pub const PLAIN_CHUNK: usize = 1 << 20;

/// The bytes a download of an image of `len` bytes sends: a whole number of
/// transfers under equal-length framing, the image itself otherwise.
pub fn download_size(len: u64, framing: Framing) -> u64 {
    match framing {
        Framing::Plain => len,
        Framing::EqualLength { len: l } => len.div_ceil(l as u64) * l as u64,
    }
}

/// A size as boot loaders answer it: `0x` and hex, or decimal.
pub fn parse_number(v: &str) -> Option<u64> {
    match v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        Some(h) => u64::from_str_radix(h, 16).ok(),
        None => v.parse().ok(),
    }
}

fn plain_name(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
}

/// The kind of image a boot loader's `flash` takes `head` (the image's first
/// bytes) for, when not raw: abl `FastbootCmds.c` checks the sparse and meta
/// magic numbers and UBI's `UBI#`.
pub fn not_raw(head: &[u8]) -> Option<&'static str> {
    let magic = head.get(..4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    match magic {
        Some(0xed26_ff3a) => Some("sparse"),
        Some(0xce1a_d63c) => Some("meta"),
        _ if head.starts_with(b"UBI#") => Some("UBI"),
        _ => None,
    }
}

/// What a session may write, from the device description: checked before
/// anything is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteRules {
    /// The framing the description's mode needs (an `equal-length-transfers`
    /// departure, else plain). A session in another framing writes nothing.
    pub framing: Framing,
    /// Partitions never written (the description's never-write classes).
    pub never_write: Vec<String>,
    /// The `oem` verbs the description allows (`reboot-recovery`).
    pub oem: Vec<String>,
}

/// Names abl's `flash` takes for something other than a partition, by
/// prefix (abl `FastbootCmds.c`, `CmdFlash`): never sent, whatever the rules.
pub const NOT_PARTITIONS: &[(&str, &str)] = &[
    (
        "partition",
        "the boot loader takes any name that starts with \"partition\" for the partition table and erases the whole eMMC first",
    ),
    (
        "avb_custom_key",
        "the boot loader stores a name that starts with \"avb_custom_key\" as a verified-boot key, not in a partition",
    ),
];

impl WriteRules {
    /// Refuses `part` unless it is one plain partition name the rules allow:
    /// never one that starts like one of [`NOT_PARTITIONS`] (any case: the
    /// partition table, a verified-boot key), never one on
    /// [`WriteRules::never_write`] (any case), and never a name without a
    /// slot suffix whose `_a` or `_b` is on it (an A/B boot loader adds the
    /// active slot's suffix to such a name).
    pub fn check_partition(&self, part: &str) -> Result<(), FbError> {
        let lower = part.to_ascii_lowercase();
        if let Some((_, why)) = NOT_PARTITIONS.iter().find(|(p, _)| lower.starts_with(p)) {
            return Err(FbError::RefusedWrite(format!("flash:{part}: {why}")));
        }
        if !plain_name(part) || "flash:".len() + part.len() > COMMAND_MAX {
            return Err(FbError::RefusedWrite(format!("{part:?} is not one partition name")));
        }
        let refused = |n: &str| self.never_write.iter().any(|r| r.eq_ignore_ascii_case(n));
        if refused(&lower) {
            return Err(FbError::RefusedWrite(format!("{part} is never written (device description)")));
        }
        for slot in ["_a", "_b"] {
            if refused(&format!("{lower}{slot}")) {
                return Err(FbError::RefusedWrite(format!(
                    "{part}{slot} is never written, and an A/B boot loader may write {part} as {part}{slot}"
                )));
            }
        }
        Ok(())
    }

    /// Refuses a session whose framing isn't the one the description needs.
    pub fn check_framing(&self, framing: Framing) -> Result<(), FbError> {
        if framing != self.framing {
            return Err(FbError::RefusedWrite(format!(
                "the session uses {} framing; the device description needs {} for writes",
                framing.name(),
                self.framing.name()
            )));
        }
        Ok(())
    }
}

/// How a [`Session::flash_raw`] failed: what reached the partition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteError {
    /// Refused before the download: at most reads were sent; the partition
    /// is untouched, and the session goes on unless it says it ended.
    Refused(FbError),
    /// The download failed or didn't check out; `flash:` was never sent, so
    /// the partition is untouched. The session has ended.
    BeforeFlash(FbError),
    /// `flash:` was sent and not answered OKAY: the partition may hold the
    /// image, part of it or what it held before.
    Flash(FbError),
}

impl WriteError {
    /// True when the partition may have changed.
    pub fn partition_may_have_changed(&self) -> bool {
        matches!(self, WriteError::Flash(_))
    }

    pub fn error(&self) -> &FbError {
        match self {
            WriteError::Refused(e) | WriteError::BeforeFlash(e) | WriteError::Flash(e) => e,
        }
    }
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::Refused(e) => write!(f, "not written: {e}"),
            WriteError::BeforeFlash(e) => write!(f, "not written, the download failed: {e}"),
            WriteError::Flash(e) => write!(f, "the write may have changed the partition: {e}"),
        }
    }
}

impl std::error::Error for WriteError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies() {
        assert!(parse_reply(b"OK").is_err());
        assert!(parse_reply(b"WHATxx").is_err());
        assert!(parse_reply(&[b'O'; 300]).is_err());
        assert_eq!(parse_reply(b"OKAYNoteAir").unwrap(), Reply::Okay("NoteAir".into()));
        assert_eq!(parse_reply(b"DATA000ffff0\0").unwrap(), Reply::Data(0xffff0));
    }

    #[test]
    fn only_plain_reads() {
        for ok in [
            "product",
            "hw-revision",
            "partition-size:boot_a",
            "partition-type:userdata",
            "max-download-size",
        ] {
            assert!(readable_variable(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "all",
            "serialno",
            "partition-size:",
            "a:b",
            "flash:boot",
            "x flash",
            "x\n",
            "partition-size:a:b",
            "product\0",
            "partition-size:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(readable_variable(bad).is_err(), "{bad:?}");
        }
    }
}
