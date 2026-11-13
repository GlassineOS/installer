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

The decision behind this is
[0014](https://docs.glassine.org/decisions/0014-installation-and-provisioning/); the first
spec is [`specs/001-install-from-stock`](specs/001-install-from-stock/spec.md).

## Where it runs

For devices: on a computer connected to them, from Linux, Windows or a browser. For a PC:
on the PC itself, started from removable media.

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
