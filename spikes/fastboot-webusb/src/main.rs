// SPDX-License-Identifier: GPL-2.0-or-later
//! Native spike tool: `fbspike VAR...` reads fastboot variables (read-only).
#[cfg(not(target_arch = "wasm32"))]
fn main() {
    use fbspike::{getvar, usb};
    use nusb::MaybeFuture;
    let names: Vec<String> = std::env::args().skip(1).collect();
    let names = if names.is_empty() { vec!["product".to_string()] } else { names };
    let sel = usb::selector();
    let info = nusb::list_devices()
        .wait()
        .expect("list usb devices")
        .find(|d| d.matches(&sel));
    let Some(info) = info else {
        eprintln!("no fastboot device");
        std::process::exit(2);
    };
    eprintln!("device {:04x}:{:04x}", info.vendor_id(), info.product_id());
    let dev = info.open().wait().expect("open");
    let mut t = usb::UsbTransport::open_blocking(dev).expect("claim");
    for n in &names {
        match pollster::block_on(getvar(&mut t, n)) {
            Ok((v, info)) => {
                for i in info {
                    println!("({n}) {i}");
                }
                println!("{n}: {v}");
            }
            Err(e) => println!("{n}: {e}"),
        }
    }
}
#[cfg(target_arch = "wasm32")]
fn main() {}
