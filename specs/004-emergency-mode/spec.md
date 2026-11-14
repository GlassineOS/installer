# Feature Specification: Reading a device in its emergency mode, with the person's own programmer

**Feature branch:** `emergency-mode`
**Created:** 2026-11-13 · **Status:** spec, for review
**Builds on:** [001-install-from-stock](../001-install-from-stock/spec.md) (FR-009 the
full backup), [002-device-description](../002-device-description/spec.md),
[003-check](../003-check/spec.md) · **Decisions:**
[docs 0014](https://docs.glassine.org/decisions/0014-installation-and-provisioning/),
[docs 0008](https://docs.glassine.org/decisions/0008-bring-your-own-stock-kernel/) ·
**Guide:** [which loader fits](https://docs.glassine.org/guides/qualcomm-boot-and-edl/#which-loader-fits)

## Summary

Some devices can't be backed up through their own boot loader before anything changes:
the maker's boot loader can't start a system sent over USB, or isn't open at all. Many
such devices have an emergency mode in the chip's boot ROM that runs a small program sent
over USB, before any boot loader. The installer uses that mode to read the device's
storage.

Each chip family has its own emergency mode, its own protocol and its own rules for
which programs it runs. The installer reaches them through **routes**: one per kind of
emergency mode, each with its own recognition, fit check and reads. A device description
says which route its device takes and what that route needs from the person. Everything
the person sees (what is needed, how to find it, whether it fits) is the same flow for
every route, worded from the description.

Where a route needs a programmer the chip maker signed, the person supplies it for their
own device. GlassineOS doesn't build, ship, host or link such programs. The installer
guides the person to the right file, checks it, and only reads through it.

The first route is Qualcomm's emergency download mode; the first device is the Onyx
Boox Note Air (SDM636). It reaches the mode from stock Android with USB debugging on,
without root and without unlocking. Writing is a later feature with its own spec and its
own failure tests.

## User scenarios

### Scenario 1 – Back up a stock Note Air before anything changes (P1)

**Given** a Note Air on stock Android with USB debugging on, and a programmer file for
it on the computer,
**When** the person starts the backup and picks that file,
**Then** the installer checks the file, restarts the device into its emergency mode,
sends the programmer, reads every disk the installation will write to into one full
image each, checks each image, restarts the device into stock Android, and says where
the images are. Nothing on the device has changed.

### Scenario 2 – Find the right programmer (P1)

**Given** a device whose route needs a programmer, and no file picked yet,
**When** the plan reaches the backup,
**Then** the installer explains, in the description's words, what the file is, why
GlassineOS can't supply it, and what it must match. It shows the name the file is
usually found under as text the person can copy with one action, and asks them to
search for it themselves. It names no site and opens no page.

### Scenario 3 – Check a file before anything is sent (P1)

**Given** a picked file,
**When** the installer checks it,
**Then** it says one of: a known good file for this device; a file for this chip and key
that isn't known (with a warning, and sent only if the person confirms); a file for
another chip or key, or not a programmer at all (refused). Once the device is in its
emergency mode, the installer compares the chip and key the boot ROM reports with the
file before sending it.

### Scenario 4 – No programmer at hand (P2)

**Given** a device that needs a programmer for its backup,
**When** the person has none,
**Then** they can stop and come back later, or go on without the backup only by the
explicit opt-out (001 FR-009a).

### Scenario 5 – An interrupted read (P2)

**Given** a read in progress,
**When** the cable is pulled or the computer stops,
**Then** the partial image is marked incomplete and never offered as a backup, and the
person is told how to restart the device into the system it had.

## Edge cases

- The device reports a chip or key the description doesn't name: the installer sends
  nothing.
- The boot ROM refuses the file, or the programmer starts but doesn't answer, or answers
  in a way the route doesn't know: the installer stops, restarts the device, and reports
  the file as not working for this device.
- A device whose route needs no supplied file: the guide and file steps are skipped.
- The mode is reached without Android (special hardware only): 001's special-hardware
  edge case applies.
- The computer needs a one-time driver or permission to reach the mode (Windows driver,
  Linux device rule): the plan says so up front.

## Requirements

- **FR-001**: A device description names the device's emergency route, how the device
  enters and leaves the mode, and the chip and signing key its boot ROM reports. A new
  kind of emergency mode is a new route; a new device on a known route is a description
  only.
- **FR-002**: Where a route needs a supplied programmer, the description gives the
  guidance text, the name the file is usually found under, and the checksums of files
  known to work on that device. It never contains or names a place to get a file.
- **FR-003**: The programmer comes only from the person: the installer asks for a file
  and uses it for that session. GlassineOS doesn't ship, download, host or link any
  programmer, and the installer doesn't upload the file anywhere.
- **FR-004**: The installer checks a picked file before sending it: its checksum against
  the known ones, then that the route recognises it as a programmer for the reported
  chip and key. An unknown checksum is sent only after the person confirms a warning
  that says why it matters; a file for another chip or key is never sent.
- **FR-005**: The installer never builds or sends a program of its own.
- **FR-006**: A programmer counts as working for a device only once the device has
  accepted it and it answers the installer's first request; until then nothing is read.
- **FR-007**: Through the programmer the installer only lists and reads storage and
  restarts the device. It has no way to issue a write, erase or any other change, proven
  by its source and by a test that records every request it sends.
- **FR-008**: Every read is checked: each image is read back a second time, or checked
  against a hash the programmer reports, and a mismatch makes the image incomplete.
- **FR-009**: The guidance, the file step and the check are one flow for every route;
  only the description's words and values differ.
- **FR-010**: Everything works from a browser over WebUSB as from the command line.

## Success criteria

- **SC-001**: A stock Note Air is fully backed up through this mode with a programmer the
  person supplied, and its storage reads back identical before and after.
- **SC-002**: No GlassineOS repository, release or page contains or links a programmer
  file, or a search for one.
- **SC-003**: A person with no file gets from the guide to a checked file without help
  beyond what the installer shows.
- **SC-004**: A file that doesn't fit (other chip or key, refused, silent) is reported as
  such, and the device starts its own system again afterwards. A file with an unknown
  checksum is never sent without the person's confirmation.
- **SC-005**: Adding a device on a known route needs no change to the installer, only a
  description.
