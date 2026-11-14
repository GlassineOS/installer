# Feature Specification: Reading a device in its emergency mode, with the person's own programmer

**Feature branch:** `emergency-mode`
**Created:** 2026-11-13 · **Status:** spec, for review
**Builds on:** [001-install-from-stock](../001-install-from-stock/spec.md) (FR-009 the
full backup), [002-device-description](../002-device-description/spec.md),
[003-check](../003-check/spec.md) · **Decisions:**
[docs 0014](https://docs.glassine.org/decisions/0014-installation-and-provisioning/),
[docs 0008](https://docs.glassine.org/decisions/0008-bring-your-own-stock-kernel/)

## Summary

Some devices can't be backed up through their own boot loader before anything changes:
the maker's boot loader can't start a system sent over USB, or isn't open at all. Many
such devices have an emergency mode in the chip's boot ROM that runs a small program,
signed for that chip, sent over USB before any boot loader. The installer uses that mode
to read the device's storage with a programmer the person supplies for their own device.
GlassineOS doesn't build, ship or host such programs, and sends only programs the device
itself accepts as signed.

The first target is the Onyx Boox Note Air (Qualcomm SDM636). It reaches the mode from
stock Android with USB debugging on, without root and without unlocking.

The installer only reads through the programmer: it lists and reads storage, checks what
it read, and restarts the device. Writing is a later feature with its own spec and its
own failure tests.

## User scenarios

### Scenario 1 – Back up a stock Note Air before anything changes (P1)

**Given** a Note Air on stock Android with USB debugging on, and a programmer file for
its chip on the computer,
**When** the person starts the backup and picks that file,
**Then** the installer restarts the device into its emergency mode, sends the
programmer, reads every disk the installation will write to into one full image each,
checks each image, restarts the device into stock Android, and says where the images
are. Nothing on the device has changed.

### Scenario 2 – Find out whether a programmer fits, before anything is read (P1)

**Given** a device in its emergency mode and a programmer file,
**When** the installer inspects them,
**Then** it reads what the boot ROM reports about the chip without sending anything,
says whether the file is made for that chip and signing key, and only then sends it. A
file the boot ROM refuses, or one that doesn't answer once started, is reported as not
fitting this device; the device is restarted and nothing else is tried.

### Scenario 3 – No programmer at hand (P2)

**Given** a device that needs this mode for its backup,
**When** the person has no programmer file,
**Then** the plan says which chip and signing key the file must fit, that GlassineOS
can't supply it, and that the installation can go on without the backup only by the
person's explicit opt-out (001 FR-009a).

### Scenario 4 – An interrupted read (P2)

**Given** a read in progress,
**When** the cable is pulled or the computer stops,
**Then** the partial image is marked incomplete and never offered as a backup, and the
person is told how to restart the device into the system it had.

## Edge cases

- The device reports a chip the description doesn't name: the installer sends nothing.
- The programmer starts but answers in a way the installer doesn't know: it stops,
  restarts the device, and reports the file as unsupported.
- The mode is reached without Android (special hardware only): 001's special-hardware
  edge case applies.
- The computer needs a one-time driver or permission to reach the mode (Windows driver,
  Linux device rule): the plan says so up front.

## Requirements

- **FR-001**: The installer recognises a device in its emergency mode from what the boot
  ROM reports (chip, signing key), before sending anything.
- **FR-002**: The programmer comes only from the person: the installer asks for a file
  and uses it for that session. GlassineOS doesn't ship, download, host or point to a
  download of any programmer, and the installer doesn't upload it anywhere.
- **FR-003**: Before sending a file, the installer checks that it is a programmer for the
  reported chip and signing key, and sends nothing else. The installer never builds or
  sends a program of its own.
- **FR-004**: A programmer counts as working for a device only once the device has
  accepted it and it answers the installer's first request; until then nothing is read.
- **FR-005**: Through the programmer the installer only lists and reads storage and
  restarts the device. It has no way to issue a write, erase or any other change, proven
  by its source and by a test that records every request it sends.
- **FR-006**: Every read is checked: each image is read back a second time, or checked
  against a hash the programmer reports, and a mismatch makes the image incomplete.
- **FR-007**: The device description says which chip and signing key the device has, how
  it enters the mode, how it leaves it, and which programmer protocol it speaks; it never
  names or contains a programmer file.
- **FR-008**: Everything works from a browser over WebUSB as from the command line.

## Success criteria

- **SC-001**: A stock Note Air is fully backed up through this mode with a programmer the
  person supplied, and its storage reads back identical before and after.
- **SC-002**: No GlassineOS repository, release or page contains or links a programmer
  file.
- **SC-003**: A file that doesn't fit (other chip, other key, refused, silent) is
  reported as such, and the device starts its own system again afterwards.
