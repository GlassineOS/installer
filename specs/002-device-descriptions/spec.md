# Feature Specification: Device descriptions and the no-write check

**Feature branch:** `device-descriptions`
**Created:** 2026-11-12 · **Status:** spec, for review; nothing built
**Builds on:** [001-install-from-stock](../001-install-from-stock/spec.md) (Scenario 2, the
check; FR-001 to FR-005) · **Decision:**
[docs 0014](https://docs.glassine.org/decisions/0014-installation-and-provisioning/)

## Summary

Everything the installer knows about a machine comes from a **device description**. The
flow never names a device. This feature defines what a description holds, how the
installer finds the one that fits a machine, and the check that uses it: inspect the
machine and produce a plan, writing nothing.

Two targets shape the format, because they differ in almost everything:

- **The Onyx Boox Note Air**: starts from an Android bootloader, has fixed partitions on
  eMMC, vendor files the system needs, values unique to the unit, and stock Android to
  keep or replace.
- **A PC that starts from UEFI**, first as a QEMU virtual machine with UEFI firmware: any
  disk, any other operating systems, boot entries in the firmware.

PCs aren't described one model at a time. One general description covers every UEFI PC,
and the check finds out what each one has. A description for a particular model only adds
what that model needs beyond it.

The same check, run in CI on virtual PCs and on recorded Note Air data, proves it never
writes.

## User scenarios

### Scenario 1 – Check a Note Air in its stock state (P1)

**Given** a Note Air with stock Android, connected to the computer,
**When** the person runs the check,
**Then** the installer recognises it from its hardware and partition table, and shows:
the partitions it would never write, back up, replace and create; the vendor files it
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
named system), and says what happens to the firmware's boot entries. A disk that is too
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
**Then** every rule of this spec that a description can break is reported before the
description is used (an unclassified field, a never-write partition also marked replace, a
capability the backend doesn't know), and the check recognises the device without any
change to the installer itself.

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

### What a description holds

- **FR-001**: A description has an id, a version, a name for people, and the platform
  family it belongs to (devices that start from an Android bootloader, PCs that start from
  UEFI). Each family has exactly one backend.
- **FR-002**: **Recognition**: the hardware and firmware signals that identify the device
  and the values they must have. A user-visible model name may be one signal, never the
  only one. Each signal says which modes of the machine can provide it.
- **FR-003**: **Capabilities**: the capabilities the device has, from a list the backends
  define (for example: start a chosen kernel, a one-shot trial start, a recovery, keep the
  other system, capture unit values). Each is required or optional for an installation;
  a missing required one refuses the installation (001 FR-016).
- **FR-004**: **Storage**: for a device with fixed partitions, every known partition with
  one class: never-write, back-up-and-replace, keep, create, or resize. For a PC, the rules
  the backend uses to propose a place on any disk. Anything unclassified is never written.
- **FR-005**: **Device data**: the vendor files the system needs, referred to by the
  device's own list ([boot 006](https://github.com/GlassineOS/boot/blob/main/specs/006-device-data-storage-access/spec.md)),
  not a second copy of it. The check reports each file present or missing; it reads only
  what it must to tell.
- **FR-006**: **Unit values**: each value unique to the unit the installed system needs and
  can't read itself, with the mode it is read from and where the installed system expects
  it. Values are never part of a report or a shared file.
- **FR-007**: **Escapes**: for each step the backend may take on the device, how to undo it
  and what that depends on, including special hardware.
- **FR-008**: **Texts**: what the installer shows the person about this device (how to
  connect it, how to put it in a mode), so no screen of the flow is device-specific.
- **FR-009**: A description holds no vendor file, no part of one, and no value of a
  particular unit.

### Finding the description

- **FR-010**: The check reads the signals the machine's current mode offers, without
  writing, and compares them with every description of its platform family.
- **FR-011**: The most specific description that fits wins (a model's description over the
  general PC one). Two that fit equally, or a fit with contradicting signals, means
  unsupported (Scenario 4).
- **FR-012**: The general PC description fits every machine that starts from UEFI on a CPU
  architecture GlassineOS builds for.

### The check

- **FR-013**: The check writes nothing: no partition, no partition table, no boot setting,
  no firmware variable, no file on the machine. It doesn't mount any file system writable.
- **FR-014**: Its result is the plan of 001 FR-003, or the report of Scenario 3, both as
  screens and as a record a program reads, versioned.
- **FR-015**: Reports for sharing (Scenario 3) are built from the record with unit values,
  serial numbers, user names and file names left out.

### Checking descriptions

- **FR-016**: The rules a description can break (Scenario 6) are checked by a tool that
  runs in CI for every description, and by the installer before it uses one.

## Key entities

- **Device description**: as FR-001 to FR-009.
- **Signal**: one piece of hardware or firmware identity, with the modes that provide it.
- **Capability**: a named ability a backend knows how to use.
- **Check record**: the versioned result of a check (a plan or a report).

## Success criteria

- **SC-001**: In CI, the check runs on virtual UEFI PCs with each disk of Scenario 2, and
  every disk image and firmware variable store is byte-identical before and after.
- **SC-002**: In CI, the check runs on recorded Note Air data (partition table, names,
  sizes, identity signals; no partition contents, no vendor file, no unit value) and gives
  the same plan as on the device.
- **SC-003**: On a real Note Air in its stock state, run with the owner's go, every
  partition reads back with the same checksum before and after the check.
- **SC-004**: A machine no description fits gets a report in every CI case of Scenario 3,
  and no report contains a value FR-015 leaves out (checked by a scan in CI).
- **SC-005**: The Note Air and the general PC description pass the description checker,
  and the flow has no screen or check specific to either.

## Assumptions

- The first general PC description covers x86-64 (the north star); other architectures that
  start from UEFI can be added as signals, not new descriptions.
- Where descriptions live: each device's own description with its device repository (0014);
  the general PC description with the installer, as part of its backend.
- Recorded device data for CI is made with the owner's go from the owner's device, and holds
  only what SC-002 lists.
