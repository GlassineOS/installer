// SPDX-License-Identifier: GPL-2.0-or-later
//! Spike: the fastboot protocol in one Rust core, over nusb natively and over
//! WebUSB in the browser. Read-only by construction: the only command is
//! `getvar`. There is no download, flash, erase or reboot here.

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
}

impl fmt::Display for FbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FbError::Usb(e) => write!(f, "usb: {e}"),
            FbError::Fail(e) => write!(f, "device said FAIL: {e}"),
            FbError::Malformed(b) => write!(f, "malformed reply: {b:02x?}"),
            FbError::NotReadOnly(v) => write!(f, "refused: {v:?} is not a plain variable name"),
        }
    }
}

pub fn parse_reply(buf: &[u8]) -> Result<Reply, FbError> {
    if buf.len() < 4 || buf.len() > REPLY_MAX {
        return Err(FbError::Malformed(buf.to_vec()));
    }
    let (tag, rest) = buf.split_at(4);
    let text = String::from_utf8_lossy(rest).into_owned();
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

/// `getvar:<name>`. INFO/TEXT lines before the final reply are collected and
/// returned with it (some bootloaders answer `getvar:all` that way).
pub async fn getvar<T: Transport>(t: &mut T, name: &str) -> Result<(String, Vec<String>), FbError> {
    // A variable name never contains ':' or whitespace; refusing them keeps
    // this function from ever sending anything but a getvar.
    if name.is_empty() || name.chars().any(|c| c == ':' || c.is_whitespace() || c.is_control()) {
        return Err(FbError::NotReadOnly(name.to_string()));
    }
    t.send(format!("getvar:{name}").as_bytes()).await?;
    let mut info = Vec::new();
    loop {
        match parse_reply(&t.recv().await?)? {
            Reply::Okay(v) => return Ok((v, info)),
            Reply::Fail(e) => return Err(FbError::Fail(e)),
            Reply::Info(s) | Reply::Text(s) => info.push(s),
            Reply::Data(_) => return Err(FbError::Malformed(b"DATA to getvar".to_vec())),
        }
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

    /// Called from a button click (WebUSB needs a user gesture): asks the
    /// browser for a fastboot device and reads the given variables.
    #[wasm_bindgen]
    pub async fn read_vars(names: String) -> Result<String, JsValue> {
        let err = |e: FbError| JsValue::from_str(&e.to_string());
        let info = nusb::request_device(&[usb::selector()])
            .await
            .map_err(|e| JsValue::from_str(&e.to_string()))?
            .ok_or_else(|| JsValue::from_str("no device chosen"))?;
        let dev = info.open().await.map_err(|e| JsValue::from_str(&e.to_string()))?;
        let mut t = usb::UsbTransport::open(dev).await.map_err(err)?;
        let mut out = String::new();
        for n in names.split_whitespace() {
            match getvar(&mut t, n).await {
                Ok((v, info)) => {
                    for i in info {
                        out.push_str(&format!("({n}) {i}\n"));
                    }
                    out.push_str(&format!("{n}: {v}\n"));
                }
                Err(e) => out.push_str(&format!("{n}: {e}\n")),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

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
    fn fake(r: &[&[u8]]) -> Fake {
        Fake { sent: vec![], replies: r.iter().map(|x| x.to_vec()).collect() }
    }

    #[test]
    fn okay() {
        let mut t = fake(&[b"OKAYnoteair"]);
        let r = pollster::block_on(getvar(&mut t, "product")).unwrap();
        assert_eq!(r.0, "noteair");
        assert_eq!(t.sent, vec![b"getvar:product".to_vec()]);
    }
    #[test]
    fn info_then_okay() {
        let mut t = fake(&[b"INFOa: 1", b"TEXTb", b"OKAY"]);
        let (v, info) = pollster::block_on(getvar(&mut t, "all")).unwrap();
        assert_eq!(v, "");
        assert_eq!(info, vec!["a: 1", "b"]);
    }
    #[test]
    fn fail() {
        let mut t = fake(&[b"FAILunknown variable"]);
        assert_eq!(pollster::block_on(getvar(&mut t, "x")), Err(FbError::Fail("unknown variable".into())));
    }
    #[test]
    fn refuses_anything_but_a_name() {
        for bad in ["", "a:b", "x flash", "x\n", "product:boot"] {
            let mut t = fake(&[]);
            assert!(matches!(pollster::block_on(getvar(&mut t, bad)), Err(FbError::NotReadOnly(_))));
            assert!(t.sent.is_empty(), "sent something for {bad:?}");
        }
    }
    #[test]
    fn malformed() {
        assert!(parse_reply(b"OK").is_err());
        assert!(parse_reply(b"WHATxx").is_err());
        assert!(parse_reply(&[b'O'; 300]).is_err());
        assert_eq!(parse_reply(b"DATA00000010").unwrap(), Reply::Data(16));
    }
}
