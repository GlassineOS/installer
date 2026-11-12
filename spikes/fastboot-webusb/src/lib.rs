// SPDX-License-Identifier: GPL-2.0-or-later
//! Spike: the fastboot protocol in one Rust core, over nusb natively and over
//! WebUSB in the browser. Writes nothing to the device's storage by
//! construction: the only commands are `getvar`, `download` (into the
//! bootloader's RAM) and `reboot`. There is no flash, erase, boot or
//! set_active here.

use std::fmt;

/// Fastboot replies are at most 64 bytes in the original protocol; newer
/// bootloaders send up to 256. Read with room for either.
pub const REPLY_MAX: usize = 256;

/// The fastboot USB interface: vendor-specific class, subclass 0x42, protocol 0x03.
pub const FB_CLASS: u8 = 0xff;
pub const FB_SUBCLASS: u8 = 0x42;
pub const FB_PROTOCOL: u8 = 0x03;

#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    Okay(String),
    Fail(String),
    Info(String),
    Text(String),
    Data(u32),
}

#[derive(Debug, PartialEq, Eq)]
pub enum FbError {
    Usb(String),
    Fail(String),
    Malformed(Vec<u8>),
    NotReadOnly(String),
    /// The equal-length warm-up didn't give the expected answers: the device
    /// isn't in a fresh fastboot boot.
    NotFresh(String),
}

impl fmt::Display for FbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FbError::Usb(e) => write!(f, "usb: {e}"),
            FbError::Fail(e) => write!(f, "device said FAIL: {e}"),
            FbError::Malformed(b) => write!(f, "malformed reply: {b:02x?}"),
            FbError::NotReadOnly(v) => write!(f, "refused: {v:?} is not a plain variable name"),
            FbError::NotFresh(why) => write!(f, "not a fresh fastboot boot ({why}); restart the device into fastboot"),
        }
    }
}

pub fn parse_reply(buf: &[u8]) -> Result<Reply, FbError> {
    if buf.len() < 4 || buf.len() > REPLY_MAX {
        return Err(FbError::Malformed(buf.to_vec()));
    }
    let (tag, rest) = buf.split_at(4);
    // Some bootloaders end the reply with a NUL (the Note Air after DATA).
    let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
    let text = String::from_utf8_lossy(&rest[..end]).into_owned();
    match tag {
        b"OKAY" => Ok(Reply::Okay(text)),
        b"FAIL" => Ok(Reply::Fail(text)),
        b"INFO" => Ok(Reply::Info(text)),
        b"TEXT" => Ok(Reply::Text(text)),
        b"DATA" => u32::from_str_radix(&text, 16)
            .map(Reply::Data)
            .map_err(|_| FbError::Malformed(buf.to_vec())),
        _ => Err(FbError::Malformed(buf.to_vec())),
    }
}

/// One fastboot exchange medium: a bulk OUT write and a bulk IN read.
pub trait Transport {
    fn send(&mut self, data: &[u8]) -> impl Future<Output = Result<(), FbError>>;
    fn recv(&mut self) -> impl Future<Output = Result<Vec<u8>, FbError>>;
}

/// How commands and data are framed on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    /// Every command and data chunk as it is (the protocol as specified).
    Plain,
    /// Every transfer after the warm-up exactly `len` bytes: commands NUL-padded,
    /// data in chunks of `len`, the download size a multiple of `len`. For USB
    /// drivers that report the previous transfer's length as the current one
    /// (the Note Air's, GlassineOS/abl#1). `len` must not be a multiple of the
    /// packet size, so that every transfer ends in a short packet.
    EqualLength { len: usize },
}

/// The Note Air's framing (proven on its bootloader, GlassineOS/abl#1).
pub const NOTE_AIR: Framing = Framing::EqualLength { len: 65535 };

/// One fastboot connection with its framing. Only the commands below exist.
pub struct Session<T: Transport> {
    t: T,
    framing: Framing,
}

impl<T: Transport> Session<T> {
    /// Plain framing needs nothing. Equal-length framing needs the device in a
    /// fresh fastboot boot: one short real command, one dummy of `len` bytes
    /// (answered FAIL), then a padded `getvar:product` that must give the same
    /// product as the first. Anything else is refused before any download.
    pub async fn start(mut t: T, framing: Framing) -> Result<(Self, String), FbError> {
        match framing {
            Framing::Plain => {
                let mut s = Session { t, framing };
                let (p, _) = s.getvar("product").await?;
                Ok((s, p))
            }
            Framing::EqualLength { len } => {
                if len < 64 || len % 512 == 0 {
                    return Err(FbError::Usb(format!("bad equal length {len}")));
                }
                t.send(b"getvar:product").await?;
                let first = match parse_reply(&t.recv().await?) {
                    Ok(Reply::Okay(p)) => p,
                    r => return Err(FbError::NotFresh(format!("first command: {r:?}"))),
                };
                t.send(&vec![b'x'; len]).await?;
                match parse_reply(&t.recv().await?) {
                    Ok(Reply::Fail(_)) => {}
                    r => return Err(FbError::NotFresh(format!("dummy: {r:?}"))),
                }
                let mut s = Session { t, framing };
                match s.getvar("product").await {
                    Ok((p, _)) if p == first => Ok((s, p)),
                    r => Err(FbError::NotFresh(format!("check: {r:?}, first said {first:?}"))),
                }
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

    /// Reads replies until OKAY, collecting INFO/TEXT lines.
    async fn finish(&mut self) -> Result<(String, Vec<String>), FbError> {
        let mut info = Vec::new();
        loop {
            match parse_reply(&self.t.recv().await?)? {
                Reply::Okay(v) => return Ok((v, info)),
                Reply::Fail(e) => return Err(FbError::Fail(e)),
                Reply::Info(s) | Reply::Text(s) => info.push(s),
                Reply::Data(_) => return Err(FbError::Malformed(b"unexpected DATA".to_vec())),
            }
        }
    }

    /// `getvar:<name>`. INFO/TEXT lines before the final reply are returned
    /// with it (some bootloaders answer `getvar:all` that way).
    pub async fn getvar(&mut self, name: &str) -> Result<(String, Vec<String>), FbError> {
        // A variable name never contains ':' or whitespace; refusing them keeps
        // this from ever sending anything but a getvar.
        if name.is_empty() || name.chars().any(|c| c == ':' || c.is_whitespace() || c.is_control()) {
            return Err(FbError::NotReadOnly(name.to_string()));
        }
        self.command(&format!("getvar:{name}")).await?;
        self.finish().await
    }

    /// The size a download of `n` bytes takes on the wire: `n` itself, or with
    /// equal-length framing `n` rounded up to whole chunks.
    pub fn wire_size(&self, n: usize) -> usize {
        match self.framing {
            Framing::Plain => n,
            Framing::EqualLength { len } => n.div_ceil(len) * len,
        }
    }

    /// `download`: sends `data` into the bootloader's RAM (nothing is written
    /// to storage). With equal-length framing the data is zero-padded to
    /// `wire_size`; a later `flash` of it then writes those zeros too, so the
    /// partition must have room for them. Returns the size the device took.
    pub async fn download(&mut self, data: &[u8]) -> Result<usize, FbError> {
        let size = self.wire_size(data.len());
        if size == 0 || size > u32::MAX as usize {
            return Err(FbError::Usb(format!("bad download size {size}")));
        }
        self.command(&format!("download:{size:08x}")).await?;
        match parse_reply(&self.t.recv().await?)? {
            Reply::Data(n) if n as usize == size => {}
            Reply::Data(n) => return Err(FbError::Malformed(format!("DATA {n:#x} for {size:#x}").into_bytes())),
            Reply::Fail(e) => return Err(FbError::Fail(e)),
            r => return Err(FbError::Malformed(format!("{r:?} to download").into_bytes())),
        }
        match self.framing {
            Framing::Plain => {
                for c in data.chunks(1 << 20) {
                    self.t.send(c).await?;
                }
            }
            Framing::EqualLength { len } => {
                for c in data.chunks(len) {
                    if c.len() == len {
                        self.t.send(c).await?;
                    } else {
                        let mut b = vec![0u8; len];
                        b[..c.len()].copy_from_slice(c);
                        self.t.send(&b).await?;
                    }
                }
            }
        }
        self.finish().await?;
        Ok(size)
    }

    /// `reboot`: leaves fastboot and starts the device normally.
    pub async fn reboot(&mut self) -> Result<(), FbError> {
        self.command("reboot").await?;
        self.finish().await.map(|_| ())
    }
}

pub mod usb {
    //! The nusb transport, the same code natively and in the browser.
    use super::*;
    use nusb::transfer::{Bulk, Direction, EndpointType, In, Out};
    use nusb::{Device, Interface};
    #[cfg(not(target_arch = "wasm32"))]
    use nusb::MaybeFuture;

    pub struct UsbTransport {
        out: nusb::Endpoint<Bulk, Out>,
        inp: nusb::Endpoint<Bulk, In>,
        _intf: Interface,
    }

    fn usb(e: impl fmt::Display) -> FbError {
        FbError::Usb(e.to_string())
    }

    /// Find the fastboot interface and its two bulk endpoints.
    pub fn fastboot_interface(dev: &Device) -> Option<(u8, u8, u8)> {
        let cfg = dev.active_configuration().ok()?;
        for alt in cfg.interface_alt_settings() {
            if (alt.class(), alt.subclass(), alt.protocol()) != (FB_CLASS, FB_SUBCLASS, FB_PROTOCOL) {
                continue;
            }
            let mut out = None;
            let mut inp = None;
            for ep in alt.endpoints() {
                if ep.transfer_type() != Bulk::TYPE {
                    continue;
                }
                match ep.direction() {
                    Direction::Out => out = out.or(Some(ep.address())),
                    Direction::In => inp = inp.or(Some(ep.address())),
                }
            }
            if let (Some(o), Some(i)) = (out, inp) {
                return Some((alt.interface_number(), o, i));
            }
        }
        None
    }

    impl UsbTransport {
        pub async fn open(dev: Device) -> Result<Self, FbError> {
            let (n, o, i) = fastboot_interface(&dev)
                .ok_or_else(|| FbError::Usb("no fastboot interface".into()))?;
            let intf = dev.claim_interface(n).await.map_err(usb)?;
            let out = intf.endpoint::<Bulk, Out>(o).map_err(usb)?;
            let inp = intf.endpoint::<Bulk, In>(i).map_err(usb)?;
            Ok(UsbTransport { out, inp, _intf: intf })
        }

        /// Native only: blocking open, for programs without an async runtime.
        #[cfg(not(target_arch = "wasm32"))]
        pub fn open_blocking(dev: Device) -> Result<Self, FbError> {
            let (n, o, i) = fastboot_interface(&dev)
                .ok_or_else(|| FbError::Usb("no fastboot interface".into()))?;
            let intf = dev.claim_interface(n).wait().map_err(usb)?;
            let out = intf.endpoint::<Bulk, Out>(o).map_err(usb)?;
            let inp = intf.endpoint::<Bulk, In>(i).map_err(usb)?;
            Ok(UsbTransport { out, inp, _intf: intf })
        }
    }

    impl Transport for UsbTransport {
        async fn send(&mut self, data: &[u8]) -> Result<(), FbError> {
            self.out.submit(data.to_vec().into());
            self.out.next_complete().await.status.map_err(usb)
        }
        async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
            let mps = self.inp.max_packet_size();
            let len = REPLY_MAX.div_ceil(mps) * mps;
            let buf = self.inp.allocate(len);
            self.inp.submit(buf);
            let c = self.inp.next_complete().await;
            c.status.map_err(usb)?;
            Ok(c.buffer[..c.actual_len].to_vec())
        }
    }

    pub fn selector() -> nusb::DeviceSelector {
        nusb::DeviceSelector::all().with_class_subclass_protocol(FB_CLASS, FB_SUBCLASS, FB_PROTOCOL)
    }
}

#[cfg(target_arch = "wasm32")]
mod web {
    use super::*;
    use wasm_bindgen::prelude::*;

    fn js(e: impl fmt::Display) -> JsValue {
        JsValue::from_str(&e.to_string())
    }

    /// Called from a button click (WebUSB needs a user gesture): asks the
    /// browser for a fastboot device, starts a session with the framing
    /// (`equal` = the Note Air's), reads the given variables and, if
    /// `test_mib` > 0, downloads that many MiB of a pattern into the
    /// bootloader's RAM, then reboots the device if `reboot`.
    #[wasm_bindgen]
    pub async fn run(names: String, equal: bool, test_mib: u32, reboot: bool) -> Result<String, JsValue> {
        let info = nusb::request_device(&[usb::selector()])
            .await
            .map_err(js)?
            .ok_or_else(|| JsValue::from_str("no device chosen"))?;
        let dev = info.open().await.map_err(js)?;
        let t = usb::UsbTransport::open(dev).await.map_err(js)?;
        let framing = if equal { NOTE_AIR } else { Framing::Plain };
        let (mut s, product) = Session::start(t, framing).await.map_err(js)?;
        let mut out = format!("product: {product} ({framing:?})\n");
        for n in names.split_whitespace() {
            match s.getvar(n).await {
                Ok((v, info)) => {
                    for i in info {
                        out.push_str(&format!("({n}) {i}\n"));
                    }
                    out.push_str(&format!("{n}: {v}\n"));
                }
                Err(e) => out.push_str(&format!("{n}: {e}\n")),
            }
        }
        if test_mib > 0 {
            let data = pattern((test_mib as usize) << 20);
            let t0 = js_sys::Date::now();
            let n = s.download(&data).await.map_err(js)?;
            let ms = js_sys::Date::now() - t0;
            out.push_str(&format!("download: OKAY, {n} bytes into RAM in {ms:.0} ms\n"));
        }
        if reboot {
            s.reboot().await.map_err(js)?;
            out.push_str("reboot: OKAY\n");
        }
        Ok(out)
    }
}

/// Test data that isn't all zeros or a short repeat.
pub fn pattern(n: usize) -> Vec<u8> {
    (0..n as u32).map(|i| (i.wrapping_mul(2654435761) >> 24) as u8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Bootloader whose USB driver counts each OUT transfer as the length of the
    /// previous one, as the Note Air's does (GlassineOS/abl#1): the first
    /// transfer of a boot is counted right, the second as 6 bytes, every later
    /// one as the length of the one before it. Its 64-byte command buffer keeps
    /// what the last command left in it.
    struct NoteAir {
        n: usize,
        prev: usize,
        cmdbuf: [u8; 64],
        left: usize,
        got: Vec<u8>,
        replies: VecDeque<Vec<u8>>,
        rebooted: bool,
    }
    impl NoteAir {
        fn fresh() -> Self {
            NoteAir { n: 0, prev: 0, cmdbuf: [0; 64], left: 0, got: vec![], replies: VecDeque::new(), rebooted: false }
        }
        /// A device after an earlier session: not the first transfers of a
        /// boot, and an old command still in the buffer.
        fn warm(prev: usize, old: &[u8]) -> Self {
            let mut d = Self::fresh();
            d.n = 5;
            d.prev = prev;
            d.cmdbuf[..old.len()].copy_from_slice(old);
            d
        }
    }
    impl Transport for NoteAir {
        async fn send(&mut self, d: &[u8]) -> Result<(), FbError> {
            let count = match self.n {
                0 => d.len(),
                1 => 6,
                _ => self.prev,
            };
            self.n += 1;
            self.prev = d.len();
            if self.left > 0 {
                let k = count.min(self.left);
                self.got.extend_from_slice(&d[..k.min(d.len())]);
                self.left -= k;
                if self.left == 0 {
                    self.replies.push_back(b"OKAY".to_vec());
                }
                return Ok(());
            }
            let k = d.len().min(64);
            self.cmdbuf[..k].copy_from_slice(&d[..k]);
            let size = count.min(63);
            let mut cmd = self.cmdbuf[..size].to_vec();
            if let Some(z) = cmd.iter().position(|&b| b == 0) {
                cmd.truncate(z);
            }
            let cmd = String::from_utf8_lossy(&cmd).into_owned();
            let r = if cmd == "getvar:product" {
                b"OKAYNoteAir".to_vec()
            } else if let Some(h) = cmd.strip_prefix("download:") {
                self.left = usize::from_str_radix(h, 16).unwrap();
                format!("DATA{h}\0").into_bytes()
            } else if cmd == "reboot" {
                self.rebooted = true;
                b"OKAY".to_vec()
            } else {
                b"FAILunknown command".to_vec()
            };
            self.replies.push_back(r);
            Ok(())
        }
        async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
            self.replies.pop_front().ok_or(FbError::Usb("timeout".into()))
        }
    }

    fn block<F: Future>(f: F) -> F::Output {
        pollster::block_on(f)
    }

    #[test]
    fn plain_framing_fails_on_the_note_air() {
        let (mut s, p) = block(Session::start(NoteAir::fresh(), Framing::Plain)).unwrap();
        assert_eq!(p, "NoteAir");
        assert!(matches!(block(s.getvar("product")), Err(FbError::Fail(_))));
    }

    #[test]
    fn equal_length_runs_many_commands_and_a_download() {
        let (mut s, p) = block(Session::start(NoteAir::fresh(), NOTE_AIR)).unwrap();
        assert_eq!(p, "NoteAir");
        for _ in 0..3 {
            assert_eq!(block(s.getvar("product")).unwrap().0, "NoteAir");
        }
        let data = pattern(1_000_000);
        let n = block(s.download(&data)).unwrap();
        assert_eq!(n, 1_000_000usize.div_ceil(65535) * 65535);
        assert_eq!(&s.t.got[..data.len()], &data[..]);
        assert!(s.t.got[data.len()..].iter().all(|&b| b == 0));
        assert_eq!(s.t.got.len(), n);
        assert_eq!(block(s.getvar("product")).unwrap().0, "NoteAir");
        block(s.reboot()).unwrap();
        assert!(s.t.rebooted);
    }

    #[test]
    fn a_warm_device_is_refused_or_works_but_never_misreads() {
        // After an earlier padded download the buffer holds a longer command:
        // the short first command is misread and the session refuses to start.
        let d = NoteAir::warm(65535, b"download:000ffff0");
        assert!(matches!(block(Session::start(d, NOTE_AIR)), Err(FbError::NotFresh(_))));
        // After an earlier padded getvar the misread happens to give the same
        // command; the check then passes and the session works.
        let d = NoteAir::warm(65535, b"getvar:product");
        let (mut s, _) = block(Session::start(d, NOTE_AIR)).unwrap();
        assert_eq!(block(s.download(&pattern(70_000))).unwrap(), 2 * 65535);
    }

    #[test]
    fn bad_equal_lengths_are_refused() {
        for len in [0, 63, 512, 65536] {
            let r = block(Session::start(NoteAir::fresh(), Framing::EqualLength { len }));
            assert!(r.is_err(), "{len}");
        }
    }

    struct Fake {
        sent: Vec<Vec<u8>>,
        replies: VecDeque<Vec<u8>>,
    }
    impl Transport for Fake {
        async fn send(&mut self, d: &[u8]) -> Result<(), FbError> {
            self.sent.push(d.to_vec());
            Ok(())
        }
        async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
            self.replies.pop_front().ok_or(FbError::Usb("timeout".into()))
        }
    }
    fn plain(r: &[&[u8]]) -> Session<Fake> {
        let mut q: VecDeque<Vec<u8>> = r.iter().map(|x| x.to_vec()).collect();
        q.push_front(b"OKAYx".to_vec());
        block(Session::start(Fake { sent: vec![], replies: q }, Framing::Plain)).unwrap().0
    }

    #[test]
    fn okay() {
        let mut s = plain(&[b"OKAYnoteair"]);
        assert_eq!(block(s.getvar("version")).unwrap().0, "noteair");
        assert_eq!(s.t.sent[1], b"getvar:version".to_vec());
    }
    #[test]
    fn info_then_okay() {
        let mut s = plain(&[b"INFOa: 1", b"TEXTb", b"OKAY"]);
        let (v, info) = block(s.getvar("all")).unwrap();
        assert_eq!(v, "");
        assert_eq!(info, vec!["a: 1", "b"]);
    }
    #[test]
    fn fail() {
        let mut s = plain(&[b"FAILunknown variable"]);
        assert_eq!(block(s.getvar("x")), Err(FbError::Fail("unknown variable".into())));
    }
    #[test]
    fn refuses_anything_but_a_name() {
        for bad in ["", "a:b", "x flash", "x\n", "product:boot"] {
            let mut s = plain(&[]);
            assert!(matches!(block(s.getvar(bad)), Err(FbError::NotReadOnly(_))));
            assert_eq!(s.t.sent.len(), 1, "sent something for {bad:?}");
        }
    }
    #[test]
    fn download_checks_the_size_the_device_echoes() {
        let mut s = plain(&[b"DATA00000010"]);
        assert!(matches!(block(s.download(&[1; 32])), Err(FbError::Malformed(_))));
    }
    #[test]
    fn malformed() {
        assert!(parse_reply(b"OK").is_err());
        assert!(parse_reply(b"WHATxx").is_err());
        assert!(parse_reply(&[b'O'; 300]).is_err());
        assert_eq!(parse_reply(b"DATA00000010").unwrap(), Reply::Data(16));
        assert_eq!(parse_reply(b"DATA000ffff0\0").unwrap(), Reply::Data(0xffff0));
    }
}
