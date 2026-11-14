// SPDX-License-Identifier: GPL-2.0-or-later
//! What a computer needs once before the installer can reach a device over
//! USB, made from the descriptions' modes (`[[modes]] usb`): on Linux a udev
//! rules file and the one line that installs it, on Windows the USB ids that
//! need the WinUSB driver. The command line installs or prints it
//! (`glassine-install host-setup`), the screens show it when the browser
//! couldn't open or claim a device, and the copy in the repository
//! (`host/linux/`) is checked against it.

use crate::format1::Description;
use serde::Serialize;

/// The rules file's name. It sorts before systemd's `73-seat-late.rules`,
/// which applies `uaccess`, and ModemManager's `80-mm-candidate.rules`, which
/// reads `ID_MM_DEVICE_IGNORE`.
pub const RULES_FILE: &str = "70-glassine-installer.rules";
/// Where it goes.
pub const RULES_DIR: &str = "/etc/udev/rules.d";

/// Kernel drivers that bind a mode's interface on Linux before the installer
/// can, by mode kind: the browser can't take an interface from a kernel
/// driver (the command line can: libusb detaches it). qcserial binds the
/// Qualcomm emergency download mode (05c6:9008) as a serial port.
pub const HELD_BY: &[(&str, &str)] = &[("emergency-download", "qcserial")];

/// One USB id the installer talks to, with the modes that use it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsbMode {
    pub vendor_id: String,
    pub product_id: String,
    /// The interface the mode is told by (class-subclass-protocol, hex), when
    /// the description names one.
    pub interface: Option<String>,
    /// The modes' kinds, sorted, once each.
    pub kinds: Vec<String>,
    /// "description:mode" for every mode with this id and interface.
    pub modes: Vec<String>,
    /// The kernel driver to let go of it on Linux ([`HELD_BY`]).
    pub release_driver: Option<String>,
}

/// The modes of `descs` that have USB ids, one entry per id and interface,
/// in the order they first appear.
pub fn usb_modes(descs: &[&Description]) -> Vec<UsbMode> {
    let mut out: Vec<UsbMode> = Vec::new();
    for d in descs {
        for m in &d.modes {
            let Some(u) = &m.usb else { continue };
            let (v, p) = (u.vendor_id.to_ascii_lowercase(), u.product_id.to_ascii_lowercase());
            let i = u.interface.as_ref().map(|x| x.to_ascii_lowercase());
            let at = match out.iter().position(|e| e.vendor_id == v && e.product_id == p && e.interface == i) {
                Some(n) => n,
                None => {
                    out.push(UsbMode {
                        vendor_id: v,
                        product_id: p,
                        interface: i,
                        kinds: vec![],
                        modes: vec![],
                        release_driver: None,
                    });
                    out.len() - 1
                }
            };
            let e = &mut out[at];
            if !e.kinds.contains(&m.kind) {
                e.kinds.push(m.kind.clone());
                e.kinds.sort();
            }
            e.modes.push(format!("{}:{}", d.id, m.id));
            if e.release_driver.is_none() {
                e.release_driver = HELD_BY.iter().find(|(k, _)| *k == m.kind).map(|(_, drv)| drv.to_string());
            }
        }
    }
    out
}

/// "ff-42-03" as udev's `ID_USB_INTERFACES` writes it (":ff4203:").
fn interface_glob(i: &str) -> String {
    format!("*:{}:*", i.replace('-', ""))
}

/// The udev rules file for `descs`.
pub fn udev_rules(descs: &[&Description]) -> String {
    let mut out = String::from(
        "# SPDX-License-Identifier: GPL-2.0-or-later
# The GlassineOS installer's host setup for Linux, made by `glassine-install
# host-setup` from the device descriptions the installer ships, one entry per
# USB mode they name. Installed as /etc/udev/rules.d/70-glassine-installer.rules.
#
# Each mode: TAG+=\"uaccess\" lets the person logged in at this computer open the
# device, from the command line or a browser, without root (73-seat-late.rules
# applies it, so this file's number is lower). ID_MM_DEVICE_IGNORE keeps
# ModemManager from probing it. Where the description names the mode's USB
# interface, a device with the same ids but without that interface isn't matched.
#
# A kernel driver that binds a mode's interface (qcserial takes the emergency
# download mode as a serial port the moment it appears) is unbound from that
# id, and only from it. The command line could take the interface back itself
# (libusb detaches the driver); a browser can't. Not a blacklist: the module
# serves real modems. Not remove_id: it drops only ids added at run time, and
# this one is in the driver's own table. Not de-authorising the interface:
# the kernel then refuses every claim of it, the browser's too.

ACTION==\"remove\", GOTO=\"glassine_installer_end\"
SUBSYSTEM!=\"usb\", GOTO=\"glassine_installer_end\"
# The interfaces (ID_USB_INTERFACES), for a system whose 50-udev-default.rules doesn't import them.
ENV{DEVTYPE}==\"usb_device\", ENV{ID_USB_INTERFACES}==\"\", IMPORT{builtin}=\"usb_id\"
",
    );
    for m in usb_modes(descs) {
        let (v, p) = (&m.vendor_id, &m.product_id);
        out.push_str(&format!(
            "\n# {v}:{p}{}: {}\n",
            m.interface.as_ref().map(|i| format!(", interface {i}")).unwrap_or_default(),
            m.modes.join(", ")
        ));
        let iface = m
            .interface
            .as_ref()
            .map(|i| format!(", ENV{{ID_USB_INTERFACES}}==\"{}\"", interface_glob(i)))
            .unwrap_or_default();
        out.push_str(&format!(
            "ENV{{DEVTYPE}}==\"usb_device\", ATTR{{idVendor}}==\"{v}\", ATTR{{idProduct}}==\"{p}\"{iface}, TAG+=\"uaccess\", ENV{{ID_MM_DEVICE_IGNORE}}=\"1\"\n"
        ));
        if let Some(drv) = &m.release_driver {
            // On its bind event, and on any later event while it is still
            // bound (udevadm trigger after installing this file). The add and
            // bind events both carry the driver, so the second finds it gone:
            // the test keeps that from logging a failed command.
            out.push_str(&format!(
                "ENV{{DEVTYPE}}==\"usb_interface\", DRIVER==\"{drv}\", ATTRS{{idVendor}}==\"{v}\", ATTRS{{idProduct}}==\"{p}\", RUN+=\"/bin/sh -c 'test ! -e /sys/bus/usb/drivers/{drv}/%k || echo %k > /sys/bus/usb/drivers/{drv}/unbind'\"\n"
            ));
        }
    }
    out.push_str("\nLABEL=\"glassine_installer_end\"\n");
    out
}

/// The commands that load the rules and apply them to devices already
/// connected (as root).
pub const RELOAD: &[&[&str]] = &[&["udevadm", "control", "--reload-rules"], &["udevadm", "trigger", "--subsystem-match=usb"]];

/// One line for a terminal that installs `rules` with sudo, for a person who
/// has only the browser: the file travels in the line itself (base64), so it
/// needs no download and no other program than the shell's own tools.
pub fn linux_command(rules: &str) -> String {
    let reload: Vec<String> = RELOAD.iter().map(|c| format!("sudo {}", c.join(" "))).collect();
    format!(
        "echo '{}' | base64 -d | sudo tee {RULES_DIR}/{RULES_FILE} > /dev/null && {}",
        base64(rules.as_bytes()),
        reload.join(" && ")
    )
}

/// The record the screens read (`host_setup_format` 1).
#[derive(Debug, Serialize)]
pub struct HostSetup {
    pub host_setup_format: u32,
    pub linux: Linux,
    pub usb: Vec<UsbMode>,
}

#[derive(Debug, Serialize)]
pub struct Linux {
    /// Where the rules file goes.
    pub path: String,
    pub rules: String,
    /// The one line that installs it ([`linux_command`]).
    pub command: String,
    /// The same with the command line.
    pub cli: String,
}

pub fn record(descs: &[&Description]) -> HostSetup {
    let rules = udev_rules(descs);
    HostSetup {
        host_setup_format: 1,
        linux: Linux {
            path: format!("{RULES_DIR}/{RULES_FILE}"),
            command: linux_command(&rules),
            rules,
            cli: "sudo glassine-install host-setup".into(),
        },
        usb: usb_modes(descs),
    }
}

/// Standard base64 with padding.
pub fn base64(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_of_the_rfc_vectors() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(i.as_bytes()), o);
        }
    }
}
