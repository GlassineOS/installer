// SPDX-License-Identifier: GPL-2.0-or-later
//! The fastboot protocol, read-only: the reply parser, the transport an edge
//! supplies, the framings of FR-011 and a session that only sends `getvar`
//! and `reboot`. No USB and no clocks here: the transport bounds every wait
//! and answers [`FbError::Timeout`] when one runs out.
//!
//! `download`, `flash`, `boot` and `erase` don't exist in this module yet;
//! they come with the install steps and their write-failure tests.

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

/// The variables with a parameter that a read may name: `partition-size:boot`.
pub const PARAM_VARIABLES: &[&str] = &["partition-size", "partition-type", "has-slot"];

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

/// One fastboot connection after the opening, with its framing. Only reads
/// and `reboot` exist.
pub struct Session<T: Transport> {
    t: T,
    framing: Framing,
    /// The opening's answer: the product, or `None` when it answered FAIL.
    product: Option<String>,
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
        let mut s = Session { t, framing, product };
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

    async fn command(&mut self, cmd: &str) -> Result<(), FbError> {
        match self.framing {
            Framing::Plain => self.t.send(cmd.as_bytes()).await,
            Framing::EqualLength { len } => {
                let mut b = vec![0u8; len];
                b[..cmd.len()].copy_from_slice(cmd.as_bytes());
                self.t.send(&b).await
            }
        }
    }

    /// Reads replies up to OKAY (`Some`) or FAIL (`None`), skipping INFO/TEXT.
    async fn finish(&mut self) -> Result<Option<String>, FbError> {
        loop {
            match parse_reply(&self.t.recv().await?)? {
                Reply::Okay(v) => return Ok(Some(v)),
                Reply::Fail(_) => return Ok(None),
                Reply::Info(_) | Reply::Text(_) => {}
                Reply::Data(_) => return Err(FbError::Malformed(b"DATA to a read".to_vec())),
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

    pub fn into_transport(self) -> T {
        self.t
    }

    pub fn transport(&self) -> &T {
        &self.t
    }
}

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
