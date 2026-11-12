// SPDX-License-Identifier: GPL-2.0-or-later
//! Native spike tool. Writes nothing to the device's storage.
//!
//!   fbspike [--note-air] [--download MIB] [--reboot] [VAR...]
//!
//! `--note-air` uses the Note Air's equal-length framing (GlassineOS/abl#1);
//! `--download` sends MIB MiB of a pattern into the bootloader's RAM.
#[cfg(not(target_arch = "wasm32"))]
fn main() {
    use fbspike::{Framing, NOTE_AIR, Session, pattern, usb};
    use nusb::MaybeFuture;
    use std::time::Instant;
    let (mut framing, mut mib, mut reboot, mut names) = (Framing::Plain, 0usize, false, vec![]);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--note-air" => framing = NOTE_AIR,
            "--download" => mib = args.next().and_then(|v| v.parse().ok()).expect("--download MIB"),
            "--reboot" => reboot = true,
            _ => names.push(a),
        }
    }
    let sel = usb::selector();
    let info = nusb::list_devices().wait().expect("list usb devices").find(|d| d.matches(&sel));
    let Some(info) = info else {
        eprintln!("no fastboot device");
        std::process::exit(2);
    };
    eprintln!("device {:04x}:{:04x}", info.vendor_id(), info.product_id());
    let dev = info.open().wait().expect("open");
    let t = usb::UsbTransport::open_blocking(dev).expect("claim");
    let t0 = Instant::now();
    let (mut s, product) = match pollster::block_on(Session::start(t, framing)) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("start: {e}");
            std::process::exit(1);
        }
    };
    println!("product: {product} ({framing:?}, {} ms)", t0.elapsed().as_millis());
    for n in &names {
        match pollster::block_on(s.getvar(n)) {
            Ok((v, info)) => {
                for i in info {
                    println!("({n}) {i}");
                }
                println!("{n}: {v}");
            }
            Err(e) => println!("{n}: {e}"),
        }
    }
    let mut ok = true;
    if mib > 0 {
        let data = pattern(mib << 20);
        let t0 = Instant::now();
        match pollster::block_on(s.download(&data)) {
            Ok(n) => {
                let ms = t0.elapsed().as_millis().max(1);
                println!("download: OKAY, {n} bytes into RAM in {ms} ms ({} MB/s)", n as u128 / 1000 / ms);
            }
            Err(e) => {
                println!("download: {e}");
                ok = false;
            }
        }
    }
    if reboot {
        match pollster::block_on(s.reboot()) {
            Ok(()) => println!("reboot: OKAY"),
            Err(e) => {
                println!("reboot: {e}");
                ok = false;
            }
        }
    }
    std::process::exit(if ok { 0 } else { 1 });
}
#[cfg(target_arch = "wasm32")]
fn main() {}
