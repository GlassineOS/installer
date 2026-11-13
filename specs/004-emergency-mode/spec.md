# Feature Specification: Reading a device in its emergency mode, with nothing from the vendor

**Feature branch:** `emergency-mode`
**Created:** 2026-11-13 · **Status:** spec, for review; nothing built
**Builds on:** [001-install-from-stock](../001-install-from-stock/spec.md) (FR-009 the
full backup, FR-012a special hardware), [002-device-description](../002-device-description/spec.md),
[003-check](../003-check/spec.md) · **Decision:**
[docs 0014](https://docs.glassine.org/decisions/0014-installation-and-provisioning/)

## Summary

Some devices can't be backed up through their own boot loader before anything changes:
the maker's boot loader can't start a system sent over USB, or isn't open at all. Many
such devices have an emergency mode in the chip's boot ROM that runs a small program sent
over USB, before any boot loader. The installer uses that mode to read the device's
storage, with a program GlassineOS builds and may ship, never a vendor file.

The first target is the Onyx Boox Note Air (Qualcomm SDM636). It reaches the mode from
stock Android with USB debugging on, without root and without unlocking.

The program does only what the installer needs: say what it is, list and read storage,
give a hash of what it read, and restart the device. Writing is a later feature with its
own spec and its own failure tests.

## User scenarios

### Scenario 1 – Back up a stock Note Air before anything changes (P1)

**Given** a Note Air on stock Android with USB debugging on,
**When** the person starts the backup,
**Then** the installer restarts it into its emergency mode, starts the GlassineOS program,
reads every disk the installation will write to into one full image each, checks each
image against the hash the device reports, restarts the device into stock Android, and
says where the images are. Nothing on the device has changed.

### Scenario 2 – Find out whether the mode can be used, before anything is sent (P1)

**Given** a device in its emergency mode,
**When** the installer inspects it,
**Then** it reads what the boot ROM reports about the chip without sending any program,
and says whether this unit accepts a program GlassineOS built. A unit that only accepts
programs signed by its maker is reported as such, and the plan offers no route through
this mode.

### Scenario 3 – An interrupted read (P2)

**Given** a read in progress,
**When** the cable is pulled or the computer stops,
**Then** the device restarts on its own within a bounded time into the system it had, and
the partial image is marked incomplete and never offered as a backup.

## Edge cases

- The device is in the mode but the program doesn't answer: the installer restarts the
  device the way the boot ROM allows, or tells the person the button or cable that does.
- The device reports a chip the description doesn't name: the installer sends nothing.
- The mode is reached without Android (special hardware only): FR-012a applies.
- The computer needs a one-time driver to reach the mode (Windows): the plan says so up
  front.

## Requirements

- **FR-001**: The installer recognises a device in its emergency mode from what the boot
  ROM reports (chip, and whether the unit only runs maker-signed programs), before
  sending anything.
- **FR-002**: The installer sends only a program GlassineOS builds from source in its own
  repository, under a licence that lets it ship. It never asks for, reads, stores or
  uploads a vendor's program.
- **FR-003**: The program can't write the device's storage. This is proven by its source
  (no write path) and by a test that sends it every write the protocol could express and
  reads the storage back unchanged.
- **FR-004**: Every read is checked: the program reports a hash of what it read, and the
  installer compares it with the data it received.
- **FR-005**: The program restarts the device on its own if the computer stops talking to
  it for a bounded time.
- **FR-006**: The device description says which chip it has, how the device enters the
  mode, and the program that fits; the program's build is part of the release.
- **FR-007**: Everything works from a browser over WebUSB as from the command line.

## Success criteria

- **SC-001**: A stock Note Air is fully backed up through this mode with no vendor file
  on the computer, and its storage reads back identical before and after.
- **SC-002**: The program's source and build are public, and the image sent to the device
  is reproducible from them.
- **SC-003**: An interrupted read leaves the device starting its own system within one
  minute.
