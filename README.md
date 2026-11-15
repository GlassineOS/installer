# GlassineOS/installer

The guided installer for GlassineOS: from a machine as it came from its maker, a PC or an
e-ink device, to a running GlassineOS, with no commands to type. **Nothing built yet.**

## How it works

One flow for every machine: **inspect → plan → confirm → back up → install → verify**.

- **Inspect and plan** write nothing. They run on any machine, supported or not, and say
  what an installation would keep, back up and replace, and what the machine lacks.
- **Confirm** once, for the whole plan. The installer then does that plan and nothing else.
- **Back up** everything the plan replaces, to the person's computer, verified before the
  first write. Values unique to the unit are captured first.
- **Install** the GlassineOS layout: a base, the first deployment, the person's storage,
  the device's data and its recovery.
- **Verify:** an installation is done only after the device has booted GlassineOS and that
  boot has been accepted.

What differs between machines comes from a **device description** (how to recognise the
device, its capabilities, its partitions, its device data) and the **backend** of its
platform family: devices that start from an Android bootloader, and PCs that start from
UEFI. Device descriptions and recipes live in each device's bootstrap repository.
Until a device's bootstrap repository holds them, they are here: descriptions in `devices/`,
and in `layouts/` the partition layout a release carries for a device (role
`partition-layout`, kind `gpt-recipe`; GlassineOS/images puts `layouts/onyx-noteair.json`
into the Note Air's releases, pinned by commit and SHA-256).

The decision behind this is
[0014](https://docs.glassine.org/decisions/0014-installation-and-provisioning/); the first
spec is [`specs/001-install-from-stock`](specs/001-install-from-stock/spec.md).

## Where it runs

For devices: on a computer connected to them, from Linux, Windows or a browser. For a PC:
on the PC itself, started from removable media.

### Setting up the computer once

The installer talks to a device in its USB modes (its boot loader's, its emergency download
mode). The computer has to let it, once:

- **Linux:** `sudo glassine-install host-setup` installs the udev rules file
  [`host/linux/70-glassine-installer.rules`](host/linux/70-glassine-installer.rules), reloads udev
  and re-triggers the USB devices already connected. It lets the person logged in open each mode's
  USB device without root (`uaccess`), keeps ModemManager away (`ID_MM_DEVICE_IGNORE`) and makes the
  kernel's `qcserial` driver let go of the emergency download mode (05c6:9008) when it binds,
  for that id only: a browser can't take an interface from a kernel driver. `glassine-install
  host-setup --print` shows the file and what it runs, and changes nothing. In the browser, when
  the device can't be opened or claimed, Reach and Back up show one line to paste into a terminal
  that installs the same file; it carries the file in itself, so it needs no download and no
  command line installed.
- **Windows:** each USB mode needs the WinUSB driver, installed once with Zadig (Options > List
  All Devices, the device's USB ID, WinUSB, Install Driver); `glassine-install host-setup` lists
  the ids. No driver package is shipped: Windows installs only signed ones, and the devices don't
  announce WinUSB themselves.

The rules file is made from the device descriptions' `[[modes]] usb` (core `host_setup`); a
test fails when the copy here differs (`make golden` rewrites it).

## The screens

`web/` holds the installer's screens as static pages (decision 0015): Preact and htm as plain
modules (`web/vendor/`), the Interleaf web form from GlassineOS/ui (`web/interleaf/`, its source
commit in `SOURCE`), no build step, Chromium only. The design they follow is
`designs/installer/` in GlassineOS/ui. They read the core's records through one module,
`web/records/index.js`. By default that is the core itself, built for the browser (`web/pkg`, not
committed: `make web-pkg`, or `make docker-web-pkg` with the CI image's wasm-bindgen-cli): the
person picks the device in the browser's chooser, the core inspects it over WebUSB (getvar only)
and answers the check and the plan. With `?fixtures=1` the screens read the core's records for
its test facts instead (the plan golden files and `web/fixtures/reports/`). They write nothing to
any machine.

The published site is `make web-site`: the screens and `web/pkg`, nothing else from the
repository.

```
make web-serve                  # the site with the core: http://127.0.0.1:8790/ (Chromium)
node web/test/serve.js          # the checkout: http://127.0.0.1:8790/web/?fixtures=1#/plan?plan=noteair-cable-yes
make docker-web-check           # every screen with every record, and the site against simulated devices
web/tools/vendor check|libs|interleaf UI_DIR
```

## Debugging

`GLASSINE_USB_TRACE=1` makes the command line log every USB bulk transfer on stderr, whatever the
mode it talks to (fastboot, the RAM environment, the emergency download mode): time, direction,
length, result and the first 32 bytes, as text or hex. The browser has no such switch. A trace
can hold the unit's serial number (fastboot answers it to getvar, the boot ROM to its query):
check it before sharing.

## License

GPL-2.0-or-later (`LICENSE`).
