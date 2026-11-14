# Spike: fastboot over WebUSB from one Rust core

Decision [0015](https://docs.glassine.org/decisions/0015-installer-stack/) owes two spikes
before the core is built on them. This directory holds the first one and the notes of the
second. Throwaway code: it proves the stack, it is not the installer.

**Writes only where the test can undo it, by construction:** the fastboot commands are
`getvar`, `download` (into the bootloader's RAM), `reboot`, and `flash` to the partitions in
`FLASH_ALLOWED` (only the Note Air's `cache`, which GlassineOS doesn't use; any other name is
refused before anything is sent, tested). There is no `erase`, `boot` (on an A/B device it
rewrites the slot flags) or `set_active`. A variable name with
`:`, white space or control characters is refused before anything is sent (tested).

## What it is

- `src/lib.rs`: the fastboot reply parser, a `Transport` trait, one nusb transport used
  both natively and in the browser, and a `Session` with its framing: plain, or
  **equal-length** for bootloaders whose USB driver counts each transfer as the length of
  the previous one (the Note Air, [GlassineOS/abl#1](https://github.com/GlassineOS/abl/issues/1)).
  Equal-length framing starts with one short real command and one dummy, then sends every
  command NUL-padded and all data in chunks of exactly 65535 bytes; it checks the warm-up's
  answers and refuses a device that isn't in a fresh fastboot boot. A `run` entry point for
  the browser (wasm-bindgen).
- `src/main.rs`: `fbspike [--note-air] [--download MIB] [--flash PART FILE] [--reboot] [VAR...]`,
  the native tool.
- `web/index.html`: one button (WebUSB needs a user gesture), the variables, the framing,
  a download test into RAM, reboot at the end; the result and the time it took.
- The tests run the session against a simulated Note Air (its driver's counting and its
  command buffer, from the abl#1 traces): plain framing fails there at the second command;
  equal-length framing runs many commands, a download that arrives byte for byte with its
  zero padding, and a reboot.
- `Dockerfile`: the toolchain (Rust with the Windows, arm64-musl and wasm targets,
  wasm-bindgen-cli matching `Cargo.lock`).

## Build

```sh
docker build -t glassine-installer-spike:fastboot .
docker run --rm -u 0 -v "$PWD":/w -w /w glassine-installer-spike:fastboot sh -c '
  apk add -q mingw-w64-gcc
  cargo test
  cargo build --release --bin fbspike                                  # Linux, static
  cargo build --release --bin fbspike --target x86_64-pc-windows-gnu   # fbspike.exe
  cargo build --release --lib --target wasm32-unknown-unknown
  wasm-bindgen --target web --out-dir web/pkg target/wasm32-unknown-unknown/release/fbspike.wasm'
python3 -m http.server -d web 8000    # then http://localhost:8000 in a Chromium-based browser
```

## Results

| Check | Result |
|---|---|
| Protocol tests (fake transport: OKAY, INFO/TEXT then OKAY, FAIL, refusals, malformed, download size check; simulated Note Air: plain fails, equal-length works, warm device refused or correct) | 11/11 pass (with flash refusals and a flash on the simulated Note Air) |
| Same source compiles for Linux x86-64 musl, Windows x86-64, arm64 musl, wasm32 | yes, no target-specific code but the entry points |
| Linux binary | static, 656 KB; on a host with no fastboot device it enumerates and says so (exit 2) |
| Windows binary | 496 KB, cross-built; not run (no Windows machine) |
| Browser | wasm 269 KB; headless Chromium 156 loads the core and finds `navigator.usb` |
| Note Air (bootloader fastboot), native Linux binary, first command of a boot | `product: NoteAir`, 151 ms, same as the stock `fastboot` tool |
| Note Air, browser (Chrome, WebUSB, the same core as wasm), first command of a boot | `product: NoteAir` (2.7 s from the click, including the device chooser) |
| Note Air, any later command of the same boot, plain framing | fails, for the stock tool as for this one ([GlassineOS/abl#1](https://github.com/GlassineOS/abl/issues/1)) |
| Note Air, equal-length framing, native Linux binary | one fastboot boot: warm-up accepted, 5 variables, 16 MiB into RAM in 390 ms (43 MB/s), reboot; back in GlassineOS, nothing changed |
| Note Air, equal-length framing, browser (Chrome, WebUSB) | one fastboot boot: warm-up accepted, 3 variables, 16 MiB into RAM in 554 ms, reboot; 2.7 s from the click including the device chooser |
| Note Air, `flash` (native, equal-length framing) | 32 MiB random image to `cache` in one fastboot boot: OKAY in 3.1 s (33,619,455 bytes with 65,023 of zero padding), reboot. Read back from GlassineOS: image identical, padding zeros, the rest of the partition unchanged byte for byte; then the backup written back (identical) |
| Claiming the interface right after the device appears (native, Linux) | "interface is busy" for the first one or two tries, then works; the tool retries opening (up to 5 s; 3 retries were needed once) |

What the API costs: on wasm only the async calls exist, and web-sys's WebUSB bindings need
`--cfg=web_sys_unstable_apis` (`.cargo/config.toml`). Natively the same async code runs
under a trivial executor (`pollster`), and opening uses the blocking calls. Neither leaks
into the protocol code.

**What the Note Air run means for the installer:** the stack works end to end on a real
device, natively and in the browser. The Note Air's bootloader only answers the first
fastboot command of each boot, and its data transfers (`flash`, `boot`) don't work, so on
this device plain fastboot can identify, not install. With equal-length framing the same
bootloader takes many commands and full downloads, with no change to it. So fastboot can
install on the Note Air (`flash` checked byte for byte, above); the framing is a property of the device's
bootloader, so it belongs in the device description. The padding costs one: a download is
rounded up to whole 65535-byte chunks with zeros, and `flash` writes them, so an image
needs that much room in its partition (a sparse image, or a raw image smaller than the
partition by at least 64 KiB).

## Spike 2: Windows USB drivers (notes; needs a Windows machine)

WebUSB and the native tool on Windows both need the device's interface bound to WinUSB.
Windows binds it on its own only when the device says it is WinUSB-compatible through
Microsoft OS descriptors; otherwise the person needs a driver (Windows Update's "Android
bootloader interface", or a vendor's). A wrong driver (libusb/libwdi) blocks WebUSB.

- **Our own USB functions** (the GlassineOS recovery, developer mode, any installer
  endpoint on the device) are Linux gadgets, and configfs gadgets can send OS descriptors
  with a compatible id (`os_desc`, `compatible_id`; [kernel ABI][cfs]). So those can be
  driver-free on Windows by design.
- **Vendor boot loaders** (fastboot in the Note Air's ABL) send what they send. Whether
  Windows binds WinUSB to the Note Air's fastboot without a driver is the test to run.
  If not, the installer has to detect the missing driver and explain it.

To run on Windows: `fbspike.exe product version-bootloader` with the device in fastboot
mode, before and after installing the driver; and the web page in Edge or Chrome.

[cfs]: https://kernel.org/doc/Documentation/ABI/testing/configfs-usb-gadget
