# Feature Specification: The PC installer drive, from the browser

**Feature branch:** `pc-drive-page`
**Created:** 2026-11-27 · **Status:** spec, for review; built, see the README's Status
**Builds on:** [001-install-from-stock](../001-install-from-stock/spec.md) (Scenario 5, a
PC; FR-016b, Secure Boot; FR-017, where the installer runs) · **Consumes:** the published
PC releases of GlassineOS/images and the installer drive they carry
([images spec 004](https://github.com/GlassineOS/images/blob/main/specs/004-installer-drive/spec.md))
· **Decisions:** [docs 0014](https://docs.glassine.org/decisions/0014-installation-and-provisioning/),
[docs 0015](https://docs.glassine.org/decisions/0015-installer-stack/)

## Summary

On a PC, GlassineOS is installed from a USB stick started on that PC: the installer drive
(001 Scenario 5, FR-017). A person who doesn't have one yet opens the browser installer on
any computer and chooses to install on a PC. The installer then helps them make the drive:
it shows the newest published drive, lets the browser download it, checks the downloaded
file on the person's computer, and explains how to write it to a USB stick with a common
imaging program, what Secure Boot needs, and how to start the PC from the stick.

It does nothing else. A browser can't write to a USB stick's storage, so the installer
doesn't, and never acts as if it did. It doesn't set anything up on the drive either:
everything the installation needs is asked in the GlassineOS session on the PC.

## User scenarios

### Scenario 1 – Get the drive and check it (P1)

**Given** a computer with any current browser and a connection to the internet,
**When** the person opens the installer, chooses to install on a PC and downloads the drive
it shows,
**Then** the installer names the release the drive holds, its date, the file's name and its
size, and says that GlassineOS is unreviewed. After the download the person chooses the
downloaded file; the installer checks it on this computer against the size and checksum the
release publishes, shows the progress, and says that the file is the published one. Nothing
is uploaded.

### Scenario 2 – A file that isn't the published one (P1)

**Given** a downloaded file that was cut off, changed, or is another file,
**When** the person chooses it for the check,
**Then** the installer says it isn't the published file (its size, or its checksum next to
the published one), tells the person not to write it and to download it again, and doesn't
show writing it as the next step.

### Scenario 3 – Write the stick and start the PC (P1)

**Given** a checked file,
**When** the person reads on,
**Then** the installer says, for the system of the computer they are on (Windows, macOS or
Linux, which they can change), how to write the file to a USB stick with balenaEtcher,
Fedora Media Writer or Raspberry Pi Imager's custom image, and on Linux also with `dd`;
that everything on the stick is erased; that the PC starts the stick only with Secure Boot
off and how to switch it off; and how to start the PC from the stick, where the installation
then goes on.

### Scenario 4 – No drive to offer (P2)

**Given** no published release carries a drive, or the list of releases can't be read (no
connection, the host refuses for the moment),
**When** the person chooses to install on a PC,
**Then** the installer says which of the two it is and where the published releases are
listed, offers nothing else to download, and nothing has happened.

### Scenario 5 – A browser that can't reach devices (P2)

**Given** a browser that can't talk to devices over USB (the device installation needs one
that can),
**When** the person opens the installer,
**Then** making a PC drive is still offered and works in full: it needs no device access.

### Edge cases

- The person chooses a file before the download has finished: its size differs, and the
  installer says the download isn't complete yet.
- The person chooses another file while a check runs: the first check stops, and only the
  last file chosen is reported.
- The file can't be read (moved, deleted, no permission): the installer says so, with no
  verdict.
- In the GlassineOS session on a PC, started from a drive already, the installer doesn't
  offer to make one.

## Requirements

### Where it is offered

- **FR-001**: The start of the browser installer offers to install on a PC. Outside the
  GlassineOS session on a PC that leads here; in that session the PC installation itself
  stays where it is (001 Scenario 5). It is offered in every browser the screens load in,
  also one that can't reach devices over USB.

### The drive to offer

- **FR-002**: The drive offered is the newest, by publication time, of the PC releases
  GlassineOS/images publishes that carry a drive image whose size and SHA-256 the
  release publishes. Releases that are drafts, or for other machines, are never offered.
  The installer names the release (its base), its publication date, the file's name and
  size, and says the release is unreviewed and was tested only on virtual machines.
- **FR-003**: With no such release, or when the list of releases can't be read, the
  installer says which, names the page where the releases are listed, and offers no
  file. It never falls back to another source or to a file it can't check.

### Download and check

- **FR-004**: The download is the browser's own download of the published file, to the
  person's computer. The installer sends nothing anywhere: no file, no checksum, no
  choice.
- **FR-005**: The person then chooses the downloaded file. The installer checks it on
  this computer: first its size against the published size, then its SHA-256, read in
  pieces as it goes so a file of any size can be checked, with the progress shown. The
  result is one of: the published file; not the published file (cut off: the size; else
  both checksums); couldn't be read. A new choice stops a running check, and only the
  last file chosen is reported.
- **FR-006**: Only the published file leads on: the writing and starting steps are shown
  as the next steps only after its check matched. A file that doesn't match is reported
  as not to be written, with the way out (download it again); the steps are not shown as
  ready for it. Before a check, the installer says to check the file first.

### Write and start

- **FR-007**: The writing steps are given per system, Windows, macOS and Linux, for the
  system of this computer first and any of them on request: balenaEtcher, Fedora Media
  Writer, and Raspberry Pi Imager's custom image (without its customisation, which is for
  Raspberry Pi OS); on Linux also `dd`, as a line to copy, with how to find the stick's
  name first. They say that the whole stick is erased and which size it needs at least,
  and that the imaging program does the writing, not the installer.
- **FR-008**: The installer says that the PC starts the drive only with Secure Boot off,
  how to switch it off in the firmware's set-up, and that with Secure Boot on the PC
  doesn't start the drive and nothing on the PC changes (001 FR-016b, images 004
  Scenario 5).
- **FR-009**: The installer says how to start the PC from the stick (its boot menu, or the
  stick first in its start order, the stick's UEFI entry) and that the installation goes
  on there, where it asks what it needs and writes nothing to the PC before the person
  confirms. It doesn't configure the drive and asks nothing for it.
- **FR-010**: No part of this route offers, shows or reports writing a stick, or any
  progress or success of a write.

### Words and tests

- **FR-011**: The words are plain and short, and come from the same place as the other
  screens' words, so they are translated the same way. The screens follow the installer's
  existing layout and the Interleaf design.
- **FR-012**: The screen tests run without a network: the list of releases comes from
  recorded lists (a drive offered, none offered, the list refused), and a chosen file whose
  checksum differs from the published one is reported as not the published file, with
  no writing step shown as ready.

## Contracts

| Contract | Owned / consumed | Spec location |
|---|---|---|
| The published PC releases (their list, each file's name, size and SHA-256) | consumed (`images`) | images `docs/artifacts.md`, "Published PC releases" |
| The installer drive image | consumed (`images`) | images spec 004 |

## Out of scope

- Writing the stick from the browser, and any set-up of the drive.
- Downloading and checking in one step inside the page: the host of the published files
  doesn't let a page from another site read them, so the browser downloads the file and
  the person chooses it for the check. A host that allows it would make that possible.
- Drives for other machines than x86-64 UEFI PCs; older releases than the newest.

## Success criteria

- **SC-001**: A person who has never made a boot stick gets from the installer's start to
  a PC started from the drive using only the installer's words and one of the named
  imaging programs.
- **SC-002**: A file that differs from the published one in any byte is never reported as
  the published file.
