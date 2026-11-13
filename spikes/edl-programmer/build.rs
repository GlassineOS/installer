// SPDX-License-Identifier: GPL-2.0-or-later
fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let script = if std::env::var_os("CARGO_FEATURE_XBL_SEC_TRAMPOLINE").is_some() {
        "link-xblsec.ld"
    } else {
        "link.ld"
    };
    println!("cargo:rustc-link-arg=-T{dir}/{script}");
    println!("cargo:rustc-link-arg=-zmax-page-size=0x1000");
    println!("cargo:rerun-if-changed=link.ld");
    println!("cargo:rerun-if-changed=link-xblsec.ld");
}
