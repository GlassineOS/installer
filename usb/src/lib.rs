// SPDX-License-Identifier: GPL-2.0-or-later
//! The USB edge of the installer: the fastboot transport over nusb, the same
//! code natively and over WebUSB. It finds the device, claims its fastboot
//! interface (retrying while the host still reports it busy), reads the USB
//! ids from the device descriptor and bounds every transfer: natively a
//! transfer that runs out of time is cancelled; WebUSB can't cancel one, so
//! the interface is released, which aborts it, and the transport is dead.
//! The same transport reaches the RAM environment's read-only disk interface
//! (`glassine_read`): only the interface it claims differs.

use glassine_installer::fastboot::{FB_CLASS, FB_PROTOCOL, FB_SUBCLASS, FbError, REPLY_MAX, Transport};
use glassine_installer::glassine_read::{RR_CLASS, RR_PROTOCOL, RR_SUBCLASS};
use glassine_installer::inspect::UsbIds;
use nusb::transfer::{Bulk, Direction, EndpointType, In, Out, TransferError};
use nusb::{Device, Interface};
use std::fmt;
use std::time::Duration;

/// The bound on every single transfer. A getvar answers in milliseconds; a
/// 64 KiB warm-up dummy in well under a second. The RAM environment's answers
/// take the device its read of up to 16 MiB or its hash of up to 64 MiB first,
/// about a second, and each transfer of a read carries at most 1 MiB.
pub const TRANSFER_TIMEOUT: Duration = Duration::from_secs(5);

/// How long opening keeps retrying an interface the host reports busy: right
/// after the device appears that lasts one or two seconds (the spike).
pub const CLAIM_RETRY_FOR: Duration = Duration::from_secs(5);
const CLAIM_RETRY_EVERY: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// No fastboot device is connected (or, in a browser, none was chosen).
    NoDevice,
    /// More than one is connected; the installer doesn't pick one.
    Several(usize),
    /// The host kept reporting the interface busy past the retries.
    Busy(String),
    /// Not allowed to open it (a udev rule, a driver on Windows).
    Permission(String),
    Usb(String),
}

impl OpenError {
    pub fn kind(&self) -> &'static str {
        match self {
            OpenError::NoDevice => "no-device",
            OpenError::Several(_) => "several-devices",
            OpenError::Busy(_) => "busy",
            OpenError::Permission(_) => "permission",
            OpenError::Usb(_) => "usb",
        }
    }
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::NoDevice => write!(f, "no device in fastboot mode is connected"),
            OpenError::Several(n) => write!(f, "{n} devices in fastboot mode are connected; connect only the one to inspect"),
            OpenError::Busy(e) => write!(
                f,
                "the device's fastboot interface is busy ({e}); close other programs that use it (fastboot, a browser tab) and try again"
            ),
            OpenError::Permission(e) => write!(f, "not allowed to open the device ({e})"),
            OpenError::Usb(e) => write!(f, "usb: {e}"),
        }
    }
}

impl std::error::Error for OpenError {}

fn open_error(e: nusb::Error) -> OpenError {
    match e.kind() {
        nusb::ErrorKind::Busy => OpenError::Busy(e.to_string()),
        nusb::ErrorKind::PermissionDenied => OpenError::Permission(e.to_string()),
        nusb::ErrorKind::NotFound | nusb::ErrorKind::Disconnected => OpenError::NoDevice,
        _ => OpenError::Usb(e.to_string()),
    }
}

/// Which interface a transport claims.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The boot loader's fastboot.
    Fastboot,
    /// The RAM environment's read-only disk reader.
    RamRead,
}

impl Kind {
    fn triple(self) -> (u8, u8, u8) {
        match self {
            Kind::Fastboot => (FB_CLASS, FB_SUBCLASS, FB_PROTOCOL),
            Kind::RamRead => (RR_CLASS, RR_SUBCLASS, RR_PROTOCOL),
        }
    }
}

pub fn selector() -> nusb::DeviceSelector {
    kind_selector(Kind::Fastboot)
}

pub fn kind_selector(kind: Kind) -> nusb::DeviceSelector {
    let (c, s, p) = kind.triple();
    nusb::DeviceSelector::all().with_class_subclass_protocol(c, s, p)
}

/// The interface of `kind` and its two bulk endpoints.
fn bulk_interface(dev: &Device, kind: Kind) -> Option<(u8, u8, u8)> {
    let cfg = dev.active_configuration().ok()?;
    for alt in cfg.interface_alt_settings() {
        if (alt.class(), alt.subclass(), alt.protocol()) != kind.triple() {
            continue;
        }
        let (mut out, mut inp) = (None, None);
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

/// The ids and interfaces of the device descriptor and its active
/// configuration. Never the serial number.
pub fn usb_ids(dev: &Device) -> UsbIds {
    let d = dev.device_descriptor();
    let interfaces = dev
        .active_configuration()
        .map(|c| c.interface_alt_settings().map(|a| (a.class(), a.subclass(), a.protocol())).collect())
        .unwrap_or_default();
    UsbIds {
        vendor_id: d.vendor_id(),
        product_id: d.product_id(),
        interfaces,
    }
}

struct Open {
    out: nusb::Endpoint<Bulk, Out>,
    inp: nusb::Endpoint<Bulk, In>,
    _intf: Interface,
}

/// The fastboot transport. Dead after a timeout.
pub struct UsbTransport {
    open: Option<Open>,
    timeout: Duration,
}

fn endpoints(intf: Interface, o: u8, i: u8) -> Result<UsbTransport, OpenError> {
    let out = intf.endpoint::<Bulk, Out>(o).map_err(|e| OpenError::Usb(e.to_string()))?;
    let inp = intf.endpoint::<Bulk, In>(i).map_err(|e| OpenError::Usb(e.to_string()))?;
    Ok(UsbTransport {
        open: Some(Open { out, inp, _intf: intf }),
        timeout: TRANSFER_TIMEOUT,
    })
}

fn usb(e: impl fmt::Display) -> FbError {
    FbError::Usb(e.to_string())
}

fn completed(status: Result<(), TransferError>, what: &str) -> Result<(), FbError> {
    match status {
        Ok(()) => Ok(()),
        Err(TransferError::Cancelled) => Err(FbError::Timeout(what.into())),
        Err(e) => Err(usb(e)),
    }
}

impl UsbTransport {
    /// Changes the bound on every single transfer: the RAM environment's
    /// device-side hashes may take longer than a getvar's
    /// [`TRANSFER_TIMEOUT`] on a slow disk.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    fn dead() -> FbError {
        FbError::Usb("the connection was closed after a timeout".into())
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native {
    use super::*;
    use nusb::MaybeFuture;

    /// The one connected fastboot device.
    pub fn find() -> Result<nusb::DeviceInfo, OpenError> {
        find_kind(Kind::Fastboot)
    }

    /// The one connected device with an interface of `kind`.
    pub fn find_kind(kind: Kind) -> Result<nusb::DeviceInfo, OpenError> {
        let s = kind_selector(kind);
        let mut all: Vec<nusb::DeviceInfo> = nusb::list_devices().wait().map_err(open_error)?.filter(|d| d.matches(&s)).collect();
        match all.len() {
            0 => Err(OpenError::NoDevice),
            1 => Ok(all.remove(0)),
            n => Err(OpenError::Several(n)),
        }
    }

    /// Opens the device and claims its fastboot interface, retrying while the
    /// host reports it busy.
    pub fn open(info: &nusb::DeviceInfo) -> Result<(UsbTransport, UsbIds), OpenError> {
        open_kind(info, Kind::Fastboot)
    }

    /// Opens the device and claims its interface of `kind`, retrying while
    /// the host reports it busy.
    pub fn open_kind(info: &nusb::DeviceInfo, kind: Kind) -> Result<(UsbTransport, UsbIds), OpenError> {
        let start = std::time::Instant::now();
        loop {
            match try_open(info, kind) {
                Err(OpenError::Busy(_)) if start.elapsed() < CLAIM_RETRY_FOR => {
                    std::thread::sleep(CLAIM_RETRY_EVERY);
                }
                r => return r,
            }
        }
    }

    fn try_open(info: &nusb::DeviceInfo, kind: Kind) -> Result<(UsbTransport, UsbIds), OpenError> {
        let dev = info.open().wait().map_err(open_error)?;
        let ids = usb_ids(&dev);
        let (n, o, i) = bulk_interface(&dev, kind).ok_or(OpenError::NoDevice)?;
        let intf = dev.claim_interface(n).wait().map_err(open_error)?;
        Ok((endpoints(intf, o, i)?, ids))
    }

    impl Transport for UsbTransport {
        async fn send(&mut self, data: &[u8]) -> Result<(), FbError> {
            let timeout = self.timeout;
            let o = self.open.as_mut().ok_or_else(UsbTransport::dead)?;
            // transfer_blocking cancels the transfer when the wait runs out.
            let c = o.out.transfer_blocking(data.to_vec().into(), timeout);
            completed(c.status, "sending a command")
        }

        async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
            self.recv_max(REPLY_MAX).await
        }

        async fn recv_max(&mut self, max: usize) -> Result<Vec<u8>, FbError> {
            let timeout = self.timeout;
            let o = self.open.as_mut().ok_or_else(UsbTransport::dead)?;
            let mps = o.inp.max_packet_size().max(1);
            let buf = o.inp.allocate(max.max(1).div_ceil(mps) * mps);
            let c = o.inp.transfer_blocking(buf, timeout);
            completed(c.status, "waiting for an answer")?;
            Ok(c.buffer[..c.actual_len].to_vec())
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native::{find, find_kind, open, open_kind};

#[cfg(target_arch = "wasm32")]
mod web {
    use super::*;
    use std::future::poll_fn;
    use std::pin::pin;
    use std::task::Poll;
    use wasm_bindgen_futures::JsFuture;
    use wasm_bindgen_futures::wasm_bindgen::{JsCast, JsValue};

    /// A promise that resolves after `d` (setTimeout, in a page or a worker).
    fn sleep(d: Duration) -> JsFuture {
        let ms = d.as_millis().min(i32::MAX as u128) as i32;
        let p = js_sys::Promise::new(&mut |resolve, _| {
            let g = js_sys::global();
            if let Ok(f) = js_sys::Reflect::get(&g, &JsValue::from_str("setTimeout")).and_then(|f| f.dyn_into::<js_sys::Function>()) {
                let _ = f.call2(&g, &resolve, &JsValue::from(ms));
            }
        });
        JsFuture::from(p)
    }

    /// `f`, or `None` when `d` passes first.
    async fn within<F: Future>(f: F, d: Duration) -> Option<F::Output> {
        let mut f = pin!(f);
        let mut s = pin!(sleep(d));
        poll_fn(|cx| {
            if let Poll::Ready(v) = f.as_mut().poll(cx) {
                return Poll::Ready(Some(v));
            }
            if s.as_mut().poll(cx).is_ready() {
                return Poll::Ready(None);
            }
            Poll::Pending
        })
        .await
    }

    /// Asks the browser for a fastboot device (the chooser; needs a user
    /// gesture), opens it and claims its interface, retrying while it is busy.
    pub async fn request() -> Result<(UsbTransport, UsbIds), OpenError> {
        request_kind(Kind::Fastboot).await
    }

    /// The same for a device with an interface of `kind`.
    pub async fn request_kind(kind: Kind) -> Result<(UsbTransport, UsbIds), OpenError> {
        let info = nusb::request_device(&[kind_selector(kind)])
            .await
            .map_err(open_error)?
            .ok_or(OpenError::NoDevice)?;
        let mut waited = Duration::ZERO;
        loop {
            match try_open(&info, kind).await {
                Err(OpenError::Busy(_) | OpenError::Usb(_)) if waited < CLAIM_RETRY_FOR => {
                    sleep(CLAIM_RETRY_EVERY).await.ok();
                    waited += CLAIM_RETRY_EVERY;
                }
                r => return r,
            }
        }
    }

    async fn try_open(info: &nusb::DeviceInfo, kind: Kind) -> Result<(UsbTransport, UsbIds), OpenError> {
        let dev = info.open().await.map_err(open_error)?;
        let ids = usb_ids(&dev);
        let (n, o, i) = bulk_interface(&dev, kind).ok_or(OpenError::NoDevice)?;
        let intf = dev.claim_interface(n).await.map_err(open_error)?;
        Ok((endpoints(intf, o, i)?, ids))
    }

    impl UsbTransport {
        /// Lets go of the interface and waits for the browser to do it. Its
        /// failure is ignored: a device that is gone (unplugged, or
        /// restarting) has no interface to let go of. Dropping the transport
        /// does the same without waiting, and the browser's rejection then
        /// goes unhandled.
        pub async fn close(mut self) {
            if let Some(Open { out, inp, _intf }) = self.open.take() {
                drop((out, inp));
                let _ = _intf.release().await;
            }
        }

        /// After a timeout: WebUSB has no cancel, so release the interface
        /// (which aborts what is pending) and never use it again.
        fn kill(&mut self, what: &str) -> FbError {
            self.open = None;
            FbError::Timeout(what.into())
        }
    }

    impl Transport for UsbTransport {
        async fn send(&mut self, data: &[u8]) -> Result<(), FbError> {
            let timeout = self.timeout;
            let o = self.open.as_mut().ok_or_else(UsbTransport::dead)?;
            o.out.submit(data.to_vec().into());
            match within(o.out.next_complete(), timeout).await {
                Some(c) => completed(c.status, "sending a command"),
                None => Err(self.kill("sending a command")),
            }
        }

        async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
            self.recv_max(REPLY_MAX).await
        }

        async fn recv_max(&mut self, max: usize) -> Result<Vec<u8>, FbError> {
            let timeout = self.timeout;
            let o = self.open.as_mut().ok_or_else(UsbTransport::dead)?;
            let mps = o.inp.max_packet_size().max(1);
            let buf = o.inp.allocate(max.max(1).div_ceil(mps) * mps);
            o.inp.submit(buf);
            match within(o.inp.next_complete(), timeout).await {
                Some(c) => {
                    completed(c.status, "waiting for an answer")?;
                    Ok(c.buffer[..c.actual_len].to_vec())
                }
                None => Err(self.kill("waiting for an answer")),
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use web::{request, request_kind};
