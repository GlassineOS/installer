// SPDX-License-Identifier: GPL-2.0-or-later
//! `emergency-info`: what the boot ROM of a device in its emergency download
//! mode (USB 05c6:9008) reports about the chip, read in Sahara's command mode
//! (spec 004 FR-001, issue #18 step 2).
//!
//! Read-only by construction: the only Sahara calls here are
//! [`Sahara::connect`] (it waits for the boot ROM's HELLO and sends nothing)
//! and [`sahara::emergency_info`] (the answer to that HELLO choosing command
//! mode, the four queries, SWITCH_MODE and the fresh HELLO). No image is sent,
//! no reset: the device is left waiting at that fresh HELLO, and the person
//! restarts it with the power key. Every USB transfer is bounded by the
//! transport's timeout, so a silent device ends in an error, not a hang.

use glassine_installer::fastboot::{FbError, Transport};
use glassine_installer::inspect::UsbIds;
use glassine_installer::sahara::{self, InfoReport, Sahara, SaharaError};
use glassine_installer_usb as usb;

/// What the person is told on stderr once the device has answered.
pub const LEAVE: &str = "nothing was sent to the device but queries; it waits in its emergency download mode.\n\
                         To get out: hold the power key ~10 s to restart.";

/// Why reading the facts failed, in words for the person.
fn explain(e: SaharaError, waiting_for_hello: bool) -> String {
    match e {
        SaharaError::Transport(FbError::Timeout(_)) if waiting_for_hello => format!(
            "the boot ROM sent no HELLO within {} s. It sends one only when its mode starts (or after a \
             mode switch): if another program or an earlier run already talked to it, restart the device \
             (hold the power key ~10 s) and enter the emergency download mode again",
            usb::TRANSFER_TIMEOUT.as_secs()
        ),
        SaharaError::Transport(FbError::Timeout(_)) => format!(
            "the boot ROM stopped answering (no answer within {} s). Restart the device: hold the power key ~10 s",
            usb::TRANSFER_TIMEOUT.as_secs()
        ),
        e => format!("{e}. Restart the device: hold the power key ~10 s"),
    }
}

/// Reads the facts over `t` and returns what goes to stdout: the text, or
/// the JSON of [`InfoReport`] with `json`; the serial only with `serial`.
pub async fn run<T: Transport>(t: T, ids: &UsbIds, json: bool, serial: bool) -> Result<String, String> {
    let mut s = Sahara::connect(t).await.map_err(|e| explain(e, true))?;
    let f = sahara::emergency_info(&mut s).await.map_err(|e| explain(e, false))?;
    let r = InfoReport::new(&f, ids.vendor_id, ids.product_id, serial);
    Ok(if json { serde_json::to_string_pretty(&r).unwrap() + "\n" } else { r.render() })
}

/// Opening the device, in words for the emergency mode (the edge's own
/// messages speak of fastboot).
fn open_message(e: usb::OpenError) -> String {
    match e {
        usb::OpenError::NoDevice => "no device in emergency download mode (USB 05c6:9008) is connected".into(),
        usb::OpenError::Several(n) => {
            format!("{n} devices in emergency download mode are connected; connect only the one to read")
        }
        usb::OpenError::Busy(e) => format!(
            "the device's emergency download interface is busy ({e}); close other programs that use it \
             (edl, QFIL, a browser tab; on Linux ModemManager can hold its serial port) and try again"
        ),
        usb::OpenError::Permission(e) => format!(
            "not allowed to open the device ({e}): on Linux a udev rule for 05c6:9008 (or root) gives access; \
             on Windows the device needs the WinUSB driver (install it with Zadig)"
        ),
        usb::OpenError::Usb(e) => format!("usb: {e}"),
    }
}

/// The one connected device in emergency download mode, opened.
pub fn open() -> Result<(usb::UsbTransport, UsbIds), String> {
    let info = usb::find_kind(usb::Kind::Emergency).map_err(open_message)?;
    usb::open_kind(&info, usb::Kind::Emergency).map_err(open_message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use glassine_installer::sahara::cmd;
    use pollster::block_on;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    /// A scripted boot ROM; what the computer sends is kept for the test.
    struct Rom {
        incoming: VecDeque<Vec<u8>>,
        sent: Rc<RefCell<Vec<Vec<u8>>>>,
    }

    impl Transport for Rom {
        async fn send(&mut self, d: &[u8]) -> Result<(), FbError> {
            self.sent.borrow_mut().push(d.to_vec());
            Ok(())
        }
        async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
            self.recv_max(256).await
        }
        async fn recv_max(&mut self, max: usize) -> Result<Vec<u8>, FbError> {
            let mut t = self.incoming.pop_front().ok_or(FbError::Timeout("waiting for an answer".into()))?;
            if t.len() > max {
                let rest = t.split_off(max);
                self.incoming.push_front(rest);
            }
            Ok(t)
        }
    }

    fn pkt(c: u32, body: &[u32]) -> Vec<u8> {
        let mut v = c.to_le_bytes().to_vec();
        v.extend_from_slice(&((8 + 4 * body.len()) as u32).to_le_bytes());
        for w in body {
            v.extend_from_slice(&w.to_le_bytes());
        }
        v
    }

    fn hello() -> Vec<u8> {
        pkt(cmd::HELLO, &[2, 1, 0x400, 0, 0, 0, 0, 0, 0, 0])
    }

    // Synthetic values: no serial or key hash of a real unit.
    const SERIAL: u32 = 0x1234_5678;
    const HW: u64 = 0x000c_c0e1_0051_0002;

    fn script(sbl: Option<u32>, msm: u64) -> Vec<Vec<u8>> {
        let hash: Vec<u8> = (0u8..32).map(|b| b ^ 0x5a).collect();
        let mut s = vec![
            hello(),
            pkt(cmd::CMD_READY, &[]),
            pkt(cmd::EXECUTE_RESP, &[1, 4]),
            SERIAL.to_le_bytes().to_vec(),
            pkt(cmd::EXECUTE_RESP, &[2, 8]),
            msm.to_le_bytes().to_vec(),
            pkt(cmd::EXECUTE_RESP, &[3, 64]),
            [hash.clone(), hash].concat(),
        ];
        match sbl {
            Some(v) => {
                s.push(pkt(cmd::EXECUTE_RESP, &[7, 4]));
                s.push(v.to_le_bytes().to_vec());
            }
            None => s.push(pkt(cmd::END_IMAGE_TX, &[0, 0x1e])),
        }
        s.push(hello());
        s
    }

    fn ids() -> UsbIds {
        UsbIds {
            vendor_id: 0x05c6,
            product_id: 0x9008,
            interfaces: vec![(0xff, 0xff, 0xff)],
        }
    }

    fn go(script: Vec<Vec<u8>>, json: bool, serial: bool) -> (Result<String, String>, Vec<Vec<u8>>) {
        let sent = Rc::new(RefCell::new(Vec::new()));
        let rom = Rom {
            incoming: script.into(),
            sent: sent.clone(),
        };
        let r = block_on(run(rom, &ids(), json, serial));
        let s = sent.borrow().clone();
        (r, s)
    }

    fn command(p: &[u8]) -> u32 {
        u32::from_le_bytes(p[..4].try_into().unwrap())
    }

    fn hash_hex() -> String {
        (0u8..32).map(|b| format!("{:02x}", b ^ 0x5a)).collect()
    }

    #[test]
    fn text_without_the_serial_by_default() {
        let (r, _) = go(script(Some(3), HW), false, false);
        let out = r.unwrap();
        assert_eq!(
            out,
            format!(
                "emergency download device 05c6:9008\n\
                 sahara: version 2 (oldest 1)\n\
                 chip: SDM636 (msm id 000cc0e1, oem id 0051, model id 0002)\n\
                 oem pk hash: {}\n\
                 sbl version: 00000003\n",
                hash_hex()
            )
        );
        assert!(!out.contains("12345678"), "the serial must not be shown by default");
        assert!(!out.contains("serial"));
    }

    #[test]
    fn text_with_the_serial_when_asked() {
        let (r, _) = go(script(Some(3), HW), false, true);
        assert!(r.unwrap().ends_with("sbl version: 00000003\nserial: 12345678\n"));
    }

    #[test]
    fn json_shape() {
        let (r, _) = go(script(Some(3), HW), true, false);
        let out = r.unwrap();
        assert!(!out.contains("12345678"));
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "vendor_id": "05c6",
                "product_id": "9008",
                "sahara_version": 2,
                "sahara_version_min": 1,
                "hw_id": "000cc0e100510002",
                "msm_id": "000cc0e1",
                "oem_id": "0051",
                "model_id": "0002",
                "chip": "SDM636",
                "oem_pk_hash": hash_hex(),
                "sbl_version": "00000003",
            })
        );
        let (r, _) = go(script(None, 0x0012_3456_0000_0000), true, true);
        let v: serde_json::Value = serde_json::from_str(&r.unwrap()).unwrap();
        assert_eq!(v["serial"], "12345678");
        assert_eq!(v["chip"], serde_json::Value::Null);
        assert_eq!(v["sbl_version"], serde_json::Value::Null);
    }

    #[test]
    fn an_unknown_chip_and_a_refused_sbl_version() {
        let (r, _) = go(script(None, 0x0012_3456_0000_0000), false, false);
        let out = r.unwrap();
        assert!(out.contains("chip: not one this installer knows (msm id 00123456, oem id 0000, model id 0000)\n"));
        assert!(out.contains("sbl version: not reported"));
    }

    /// Exactly the queries and nothing else: HELLO_RESP choosing command
    /// mode, EXECUTE and EXECUTE_DATA for each of the four facts, and
    /// SWITCH_MODE. No image, no RESET, no DONE.
    #[test]
    fn it_sends_only_queries() {
        let (r, sent) = go(script(Some(3), HW), false, false);
        r.unwrap();
        let cmds: Vec<u32> = sent.iter().map(|p| command(p)).collect();
        let mut want = vec![cmd::HELLO_RESP];
        for _ in 0..4 {
            want.extend([cmd::EXECUTE, cmd::EXECUTE_DATA]);
        }
        want.push(cmd::SWITCH_MODE);
        assert_eq!(cmds, want);
        // The HELLO_RESP names command mode (3).
        assert_eq!(u32::from_le_bytes(sent[0][20..24].try_into().unwrap()), 3);
        // Every packet sent is a short command packet: nothing image-sized.
        assert!(sent.iter().all(|p| p.len() <= 0x30));
    }

    #[test]
    fn a_silent_device_is_an_error_not_a_hang() {
        let (r, sent) = go(vec![], false, false);
        let e = r.unwrap_err();
        assert!(e.contains("no HELLO within 5 s"), "{e}");
        assert!(e.contains("hold the power key ~10 s"), "{e}");
        assert!(sent.is_empty(), "nothing is sent before a HELLO");
        // Silent after command mode was chosen.
        let mut s = script(Some(3), HW);
        s.truncate(3);
        let (r, sent) = go(s, false, false);
        let e = r.unwrap_err();
        assert!(e.contains("stopped answering"), "{e}");
        assert!(sent.iter().all(|p| ![cmd::RESET, cmd::DONE, cmd::RESET_STATE_MACHINE].contains(&command(p))));
    }

    /// The source of this command names no call that sends an image or
    /// restarts the device (the strings are put together here so this test
    /// doesn't find itself).
    #[test]
    fn no_path_to_upload_or_reset() {
        let src = include_str!("emergency.rs");
        let code = src.split("#[cfg(test)]").next().unwrap();
        for f in ["upload", "reset", "reset_state_machine", "done"] {
            let call = format!(".{f}(");
            assert!(!code.contains(&call), "{call} in emergency.rs");
            let path = format!("sahara::{f}(");
            assert!(!code.contains(&path), "{path} in emergency.rs");
        }
    }
}
