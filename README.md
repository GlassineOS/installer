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

## License

GPL-2.0-or-later (`LICENSE`).
