# Feature Specification: The check, and proof that it writes nothing

**Feature branch:** `check`
**Created:** 2026-11-12 · **Status:** spec, for review; nothing built
**Builds on:** [001-install-from-stock](../001-install-from-stock/spec.md) (Scenario 2, the
check; FR-001 to FR-005), [002-device-description](../002-device-description/spec.md) (the
format the check reads) · **Decision:**
[docs 0014](https://docs.glassine.org/decisions/0014-installation-and-provisioning/)

## Summary

The check inspects a machine, finds the device description that fits it (002), and
produces the plan of 001 or, for a machine nothing describes, a report, writing nothing.
This feature is what the check does on real machines and how it is proven never to write.

Two targets shape it, because they differ in almost everything:

- **The Onyx Boox Note Air**: starts from an Android bootloader, has fixed partitions on
  eMMC, vendor files the system needs, values unique to the unit, and stock Android to
  keep or replace.
- **A PC that starts from UEFI**, first as a QEMU virtual machine with UEFI firmware: any
  disk, any other operating systems, boot entries in the firmware.

PCs aren't described one model at a time. One general description covers every UEFI PC
(002), and the check finds out what each one has.

The same check, run in CI on virtual PCs and on recorded Note Air data, proves it never
writes.

## User scenarios

### Scenario 1 – Check a Note Air in its stock state (P1)

**Given** a Note Air with stock Android, connected to the computer,
**When** the person runs the check,
**Then** the installer recognises it from its hardware and partition table, and shows:
the partitions it would never write, replace and create; the full image of its storage
it would back up first, with its size and where it goes; the vendor files it
needs and that each one is present; the values unique to the unit it would capture; what
happens to stock Android (kept or replaced, as the person chooses); the space it needs and
has; and each step's escape, including that a failed boot-loader step leaves only the
emergency mode with a special cable. Nothing on the device is written.

### Scenario 2 – Check a PC (P1)

**Given** a UEFI PC (a virtual machine in CI) with one of these disks: blank; holding
another Linux; holding an existing EFI system partition and another system; too small,
**When** the check runs,
**Then** the general PC description fits; the plan names every disk and every system
found, proposes where GlassineOS goes (free space, a disk the person picks, or replacing a
named system), lists a backup image for each disk it would write to and none for the
others, and says what happens to the firmware's boot entries. A disk that is too
small is refused with the space it lacks. The disks and the firmware's stored settings are
unchanged afterwards.

### Scenario 3 – A machine nothing describes (P1)

**Given** a machine no description fits (an unknown e-ink device, an ARM board, a PC that
doesn't start from UEFI),
**When** the check runs,
**Then** it reports what it could read (platform family, storage, partitions, the
signals it tried), says that no description fits, and lists which capabilities the
installer would need to learn for it. The report holds no personal data and no value
unique to the unit, and the person can share it.

### Scenario 4 – Conflicting or ambiguous matches (P2)

**Given** a machine two descriptions fit equally, or whose signals contradict the
description that fits by name,
**When** the check runs,
**Then** the machine is treated as unsupported, and the report names the descriptions and
the signals in conflict.

### Scenario 5 – A partition the description doesn't mention (P2)

**Given** a device whose partition table holds a partition its description doesn't
classify (a newer stock firmware added one),
**When** the check runs,
**Then** that partition is never written, the plan lists it as unknown, and if the plan
would need to move or overlap it, the plan is refused.

### Scenario 6 – Adding a description (P2)

**Given** someone writes a description for a new device of a platform family that has a
backend,
**When** it is checked,
**Then** once it passes the checker of 002, the check recognises the device and plans for
it without any change to the installer itself.

### Edge cases

- The Note Air connected in a mode other than stock Android (its boot-loader mode, a
  recovery): it is recognised by the signals that mode offers; capabilities that need
  stock Android running (capturing a value only it knows) are reported as unavailable from
  that mode, with the mode that provides them.
- A Note Air that already runs GlassineOS: recognised as installed (001 Scenario 7), not as
  stock.
- A PC with several disks: each is listed; none is chosen silently.
- A PC with a firmware that doesn't keep boot entries: the check can't know before trying,
  so the plan says the entry is verified at the end (001 FR-013) and names the fallback
  path on the EFI system partition.
- A virtual PC with no firmware variable store: treated like a PC whose entries don't
  stick.

## Requirements

### Finding the description

- **FR-001**: The check reads the signals the machine's current mode offers, without
  writing, and recognises the machine as 002 describes (match, conflict, ambiguous,
  undetermined, none).
- **FR-002**: The general PC description fits every machine that starts from UEFI on a CPU
  architecture GlassineOS builds for.
- **FR-003**: Where a description names vendor files the installation needs from the
  device (002 FR-018a), the check reports each present or missing and reads only what it
  must to tell. Unit values (002 FR-017) are never read by the check; it says where they
  will be read from.

### The check

- **FR-004**: The check writes nothing: no partition, no partition table, no boot setting,
  no firmware variable, no file on the machine. It doesn't mount any file system writable.
- **FR-005**: Its result is the plan of 001 FR-003, or the report of Scenario 3, both as
  screens and as a record a program reads, versioned.
- **FR-006**: Reports for sharing (Scenario 3) are built from the record with unit values,
  serial numbers, user names and file names left out.

## Key entities

- **Device description**, **signal**, **state**, **mode**: as 002.
- **Facts**: what one inspection read, kept so that the check and the plan use the same
  reading (inspecting again can cost a restart of the device's mode).
- **Check record**: the versioned result of a check (a plan or a report).

## Success criteria

- **SC-001**: In CI, the check runs on virtual UEFI PCs with each disk of Scenario 2, and
  every disk image and firmware variable store is byte-identical before and after.
- **SC-002**: In CI, the check runs on recorded Note Air data (partition table, names,
  sizes, identity signals; no partition contents, no vendor file, no unit value) and gives
  the same plan as on the device.
- **SC-003**: On a real Note Air, run with the owner's go, every partition has the same
  checksum before and after the check (read by means outside the installer, since the
  check has no way to read partitions).
- **SC-004**: A machine no description fits gets a report in every CI case of Scenario 3,
  and no report contains a value FR-006 leaves out (checked by a scan in CI).
- **SC-005**: The flow has no screen or check specific to the Note Air or the PC.

## Assumptions

- The first general PC description covers x86-64 (the north star); other architectures that
  start from UEFI can be added as signals, not new descriptions.
- Recorded device data for CI is made with the owner's go from the owner's device, and holds
  only what SC-002 lists.
