# GlassineOS/installer

The guided installer for GlassineOS: from a machine as it came from its maker, a PC or an
e-ink device, to a running GlassineOS, with no commands to type.

**Status:** the command line (`glassine-install`, below) and the browser screens (`web/`) exist,
and the core is tested against simulated devices. On a real device, only the emergency read
(reading a Note Air's storage through a programmer the person supplies, from the command line and
from the browser on Linux) has been reported as run, on one lab unit: see
[#18](https://github.com/GlassineOS/installer/issues/18) and
[#39](https://github.com/GlassineOS/installer/issues/39). The other device-facing commands
(`backup`, `install`, `resume`) and Windows are from source, untested on a device; the open points
of the emergency read are in [#31](https://github.com/GlassineOS/installer/issues/31). The files
in `specs/` are design documents, not user guides: this file and `glassine-install --help` are
the reference for what the program does.

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
UEFI. A device's own repository is the source of its description. This repository owns the
description's format ([`specs/002-device-description`](specs/002-device-description/spec.md),
schema in `schema/`), holds the PC description (`devices/uefi-pc.toml`), and ships a copy of
each other description in `devices/`, pinned by `devices/lock.toml` (the repository it comes
from, a version and a hash; the Note Air's copy is still `unpinned`, its home being
GlassineOS/bootstrap-onyx-noteair). The program embeds these copies; `--devices DIR` makes a
command read the descriptions in another directory instead. In `layouts/` is the partition
layout a release carries for a device (role `partition-layout`, kind `gpt-recipe`;
GlassineOS/images puts `layouts/onyx-noteair.json` into the Note Air's releases, pinned by
commit and SHA-256).

The decision behind this is
[0014](https://docs.glassine.org/decisions/0014-installation-and-provisioning/); the first
spec is [`specs/001-install-from-stock`](specs/001-install-from-stock/spec.md).

**Vendor data.** This repository ships none: no firmware, boot loader, programmer, partition
dump, key or serial number. A file only the person can supply (the emergency mode's programmer)
is read from the person's own computer and checked by its checksum and the chip and key it names
(not a cryptographic verification), and is sent only to the device, over USB, by the mode that
needs it, and only after that check. What a backup reads goes to a folder on the person's
computer. From source: nothing in the command line or the screens uploads such a file or the
data read from a device, and the program keeps no copy of the file (the screens hold it in the
page's memory until it is closed). Not verified by running anything or watching the network; the
installer's release files are a separate matter, read from a folder the person names.

## Where it runs

For devices: on a computer connected to them, from Linux, Windows or a browser. For a PC:
on the PC itself, started from removable media.

A PC's write steps (backup, partition table, system and boot partitions, firmware boot
entry) are the core's `pc_execute`; on Linux `pcwrite/` is the only code that writes a PC's
disk and firmware variables, and `glassine-install install --pc` runs them. They have run on
a simulated machine (`core/tests/pc_execute.rs`: a power cut after every write, a write that
reads back otherwise, a cut at every save of the record, each resumed), on a tree of files
(`pcwrite/tests`) and on virtual UEFI PCs in QEMU with OVMF (`tests/qemu`, the pc-check job:
a blank disk, a kept Windows, a run cut while it writes and resumed, a backup on a small
disk; each backed up, then started from its new boot entry, which starts a placeholder
loader; the kept Windows is then restored from its backup). Never on a real PC. The backup
image holds only the blocks in use of the ext4, btrfs, NTFS and FAT file systems it reads
(`core/src/used.rs`; tested on file systems their own tools made, `pcwrite/tests/fs.rs`) and
every other byte of the disk, zeros as zeros; `restore --pc` puts it back. Not built yet: the
boot loader is a release's `boot-loader` artifact, which no release has yet.

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

## The command line

Build it with `cargo build -p glassine-install`; `glassine-install --help` (also `-h`, `help`)
prints the full usage, which is the reference for every flag. With no arguments it prints the
usage to stderr and exits 64. Behaviour below is from source unless marked: it is untested on a
device except where #18 reports a run.

**Read only: nothing is sent to a device or written to a machine** (`inspect` and `check` ask a
device connected in fastboot mode with `getvar`, or with `--pc` read the PC they run on):

- `validate FILE... [--lock LOCKFILE]`: checks device descriptions (and the lock file).
- `inspect`: reads the connected device, or with `--pc` this PC, and prints the facts;
  `--out FACTS.json` keeps them. `--reboot` restarts the device afterwards.
- `check`: what the machine is and what an installation would need, from facts or a live read.
- `plan --dry-run`: the plan for a machine: what would be kept, backed up and replaced.
- `schema`: the description format's JSON schema.
- `supplied-check FILE`: checks a file the person supplies (see below). It reads the file and
  sends nothing.
- `emergency-info`: the chip and key hash the boot ROM of a device in its emergency download mode
  reports. It sends queries only, and leaves the device waiting in that mode.
- `host-setup --print` shows what `host-setup` would install and changes nothing. `lock-unpinned`
  lists the description copies not yet pinned; it is a maintenance command.

**Sends something to a device:**

- `backup --ram IMAGE --out DIR` (Linux): sends `download` and `boot` to a device in fastboot
  mode, to start a RAM environment that reads the storage; no other command sends those. Issue #18
  records that the boot loader of the lab Note Air did not start such an image (2026-11-13).
- `emergency-read --programmer FILE`: sends the programmer the person supplies to a device in its
  emergency download mode, then lists and reads its storage, restarts it, and never writes to it.
  `--only-gpt`, `--partition`, `--only-boot-areas` and `--find-boot-area-sizes` narrow it; on a
  device not read this way before, `--only-gpt` first (the usage text gives the order).
  `--find-boot-area-sizes` can leave a programmer hung until the power key is held (#18).
- `install`, `resume`: carry out the plan, after the person types "install"; `resume` goes on
  from the record. These write to the device, or with `--pc` to the PC they run on (its
  target disk and its firmware's boot entries; never run on a real PC yet).
- `restore --pc --backup DIR`: puts a PC's backup back onto its disk and the firmware's boot
  entries and order, after the person types "restore"; run again after a cut (never run on a
  real PC yet).
- `host-setup` (without `--print`, as root on Linux) writes a udev rules file to this computer.

`emergency-try-program` is a development command, not part of the flow, and is not in the usage.
`GLASSINE_USB_TRACE=1` logs every USB transfer.

To test: `make check` (what CI runs), `make docker-check` in the CI image. `make golden` rewrites
the golden files, the screens' reports and the schema: review its diff.

### The emergency mode

On a device whose boot loader can't run a RAM system (the Note Air's, see #18), the full backup
can go through the chip's boot ROM, in its emergency download mode (Qualcomm, USB 05c6:9008). The
program can't ship a vendor's programmer, so the person supplies one for their own device
(decision 0008). `supplied-check FILE` first verifies that file against the description: a file
known to work, one that fits the chip and key but isn't known (sent only after the person
confirms), or refused. `emergency-read` runs the same check before it sends anything, compares
what the device reports with the description, and sends nothing else. The design is in
[`specs/002-device-description`](specs/002-device-description/spec.md) (the files a description
asks for), [`specs/003-check`](specs/003-check/spec.md) and
[`specs/004-emergency-mode`](specs/004-emergency-mode/spec.md); open points are in
[#18](https://github.com/GlassineOS/installer/issues/18) and
[#31](https://github.com/GlassineOS/installer/issues/31).

## The screens

`web/` holds the installer's screens as static pages (decision 0015): Preact and htm as plain
modules (`web/vendor/`), the Interleaf web form from GlassineOS/ui (`web/interleaf/`, its source
commit in `SOURCE`), no build step, Chromium only. The design they follow is
`designs/installer/` in GlassineOS/ui. They read the core's records through one module,
`web/records/index.js`. By default that is the core itself, built for the browser (`web/pkg`, not
committed: `make web-pkg`, or `make docker-web-pkg` with the CI image's wasm-bindgen-cli): the
person picks the device in the browser's chooser, the core inspects it over WebUSB (getvar only)
and answers the check and the plan. With `?fixtures=1` the screens read the core's records for
its test facts instead (the plan golden files and `web/fixtures/reports/`). The screens through
Plan write nothing to any machine; Back up and Install (`web/screens/backup.js`, `install.js`) do
what the command line's `emergency-read` and `install` do, over WebUSB, and write into folders the
person picks.

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
