# Feature Specification: Install GlassineOS from the stock system

**Feature branch:** `install-from-stock`
**Created:** 2026-11-12 · **Status:** spec, for review; nothing built
**Decision:** [docs 0014](https://docs.glassine.org/decisions/0014-installation-and-provisioning/)
· **Builds on:** [docs 0010](https://docs.glassine.org/decisions/0010-root-and-deployments/)
(bases, deployments), [docs 0008](https://docs.glassine.org/decisions/0008-bring-your-own-stock-kernel/)
(vendor files), boot [001](https://github.com/GlassineOS/boot/blob/main/specs/001-deployments/spec.md)
(tries and acceptance), boot [006](https://github.com/GlassineOS/boot/blob/main/specs/006-device-data-storage-access/spec.md)
(device data)

## Summary

A person takes a machine as it came from its maker, a PC or an e-ink device, and ends with
GlassineOS booted on it. They don't type commands and don't need to know how the machine
starts. The installer finds out what the machine is, shows a plan of everything it will
keep, back up and replace, does exactly that plan after one confirmation, and declares
success only once GlassineOS has booted and been accepted.

The same check runs on machines GlassineOS doesn't support. It writes nothing and says what
the machine would need.

Everything device-specific comes from a device description and the backend of the device's
platform family (0014). This flow is the same for every device.

## User scenarios

### Scenario 1 – Install on a supported device (P1)

**Given** a supported device in its stock state and, for a device, a computer it is
connected to,
**When** the person starts the installer and follows its screens,
**Then** the installer recognises the device, shows the plan in plain language, asks for one
confirmation, backs up, installs, and restarts the device into GlassineOS. It reports
success only after that boot has been accepted. At no point is the person asked to type a
command, find a file, or choose a partition.

### Scenario 2 – Check any machine without changing it (P1)

**Given** any machine, supported or not,
**When** the person runs the check,
**Then** nothing on the machine is written, and the person gets a report: what the machine
is, whether GlassineOS supports it, which capabilities it has and lacks, what an
installation would keep, back up and replace, and the space it would need. For an
unsupported machine, the report says what is missing and can be shared with the people
who add devices. It holds no personal data and no value unique to the unit.

### Scenario 3 – Nothing irreplaceable is lost (P1)

**Given** a device with data unique to the unit (calibration, values the stock system knows
and GlassineOS can't read),
**When** it is installed,
**Then** never-write partitions are only read; each unit-specific value the description
names is captured before the first write and is in place in the installed system; a full
image of every disk the installation writes to (its partition table and every partition,
enough to undo any damage) is on the person's computer or a disk they chose, verified
before the first write; and the plan names each image's location and size.
**And given** too little room for the backup, or a person who doesn't want one, **when**
they switch the backup off, **then** the plan says plainly that going back to the maker's
system and repairing damage from it are then impossible, the switch is its own choice,
and the unit-specific values are still captured.

### Scenario 4 – An installation that is cut off (P1)

**Given** an installation at any step,
**When** the cable is pulled, the battery dies, or the computer crashes,
**Then** on the next start of the installer it recognises the half-installed device and
either resumes from the last completed step or restores the backup. The device always
ends in one of three states: its previous system boots, GlassineOS boots, or the
installer can finish or undo the installation. The plan named each step's escape before the
person confirmed it.

### Scenario 5 – A PC (P1)

**Given** a PC that starts from UEFI, with the installer started from removable media,
**When** the person installs,
**Then** the flow is the one of Scenario 1. The plan states, for every operating system
found on the PC, whether it is kept and still startable or replaced. Replacing one is never
the default and is confirmed by name.

### Scenario 6 – Keep the stock system (P2)

**Given** a device whose description says the stock system can be kept,
**When** the person chooses to keep it,
**Then** both systems start, and the plan says how the person picks one at start-up.

### Scenario 7 – Repair, reinstall, go back (P2)

**Given** a device with GlassineOS installed,
**When** the person starts the installer,
**Then** it recognises the installation and offers: repair (boot and recovery put back,
deployments and the person's storage kept), reinstall (a fresh deployment, the person's
storage kept), and back to stock (the backup restored). Each goes through the same
plan and confirmation.

### Scenario 8 – A new device needs no new flow (P3)

**Given** a new device whose platform family already has a backend,
**When** someone adds its description,
**Then** the installer supports it with no change to the flow or its screens beyond the
texts the description provides.

### Edge cases

- The device is recognised, but by signals that contradict each other (a model name that
  doesn't match its hardware): it is treated as unsupported, and the report shows the
  conflict.
- Not enough space for the plan: refused at the plan, before anything is written.
- A required device-data file or unit-specific value can't be read: refused at the plan,
  with which one and why.
- The computer loses the backup between backup and install (disk full, removed): the
  install doesn't start.
- Not enough room for the backup where the person chose: the plan offers another place
  (another disk) or installing without it (FR-009a).
- The backup goes to the installer's own removable media while the person chose a
  removable disk as the target: as long as inspecting can't tell which disk the installer
  runs from, the plan can't rule out imaging that disk onto itself. Open: inspecting
  names the installer's own disk, and the plan refuses it as the backup's target when it
  writes to it.
- The person cancels after confirming: the installer stops at the next point the plan names
  as safe, and offers to resume or undo.
- A step that only special hardware can escape (an emergency-mode cable): the plan asks
  whether the person has it, and without it the plan has no such step, or the device is
  refused.

## Requirements

### Inspect and plan

- **FR-001**: Inspecting writes nothing to the machine: no partition, no boot setting, no
  file.
- **FR-002**: The device is recognised from its hardware and firmware (SoC, board,
  bootloader, storage layout), never from a user-visible model name alone.
- **FR-003**: The plan lists every partition and every boot setting the installation reads,
  backs up, replaces or creates, the backup image of each disk (FR-009), every operating
  system found and what happens to it, the space needed, the capabilities the result will
  have and lack, and each step's escape.
- **FR-004**: Partitions the description doesn't classify are never written.
- **FR-005**: The plan exists both as screens a person reads and as a record a program
  reads. The installer can produce it without installing (the dry run).

### Confirm and carry out

- **FR-006**: The person confirms the whole plan once, and also by name any operating system
  that will be replaced.
- **FR-007**: The installer carries out the confirmed plan and nothing else. A step it
  can't take as planned stops the installation at that step and reports it. The installer
  never substitutes another step.
- **FR-008**: Each step's outcome is recorded as it completes, on the computer and, once the
  device can hold it, on the device, so an installation can be resumed or undone (Scenario
  4).

### Keep what can't be replaced

- **FR-009**: By default the plan backs up every disk it writes to as one full image per
  disk: the disk's partition table and all its partitions, so that any damage can be
  undone, not only what the plan replaces.
  - Each image is stored compressed. It holds only the used blocks where the file system
    lets the installer tell which those are, and every block elsewhere.
  - Each image is verified by reading it back before the first write.
  - Each image is stored somewhere other than the disks the plan changes: the person's
    computer, the installer's removable media, or another disk the person chose.
  - Disks the plan doesn't write to aren't imaged, and stay untouched.
  - The plan shows, before the person confirms, each image's disk, its size and where it
    goes.
- **FR-009a**: The backup can be switched off, on a PC and on a device, so that an
  installation also works with little free space. Switching it off is a choice of its own
  with its consequence stated (no way back to the maker's system, no repair from the
  backup), never a side effect. The unit-specific values of FR-010 are captured either way.
- **FR-010**: Unit-specific values the description names are captured before the first
  write and placed into the installed system.
- **FR-011**: Vendor files are read from the device, used, and deleted from the computer
  after the installation. They are never uploaded or kept in a cache.

### The result

- **FR-012**: The installation has the layout of 0010: the system partition with a base, the
  first deployment, the person's storage on the data partition, the device data from the
  device's list, and the GlassineOS recovery; where the plan keeps another system, its
  recovery stays reachable as the secondary.
- **FR-012a**: Where the installer makes the partitions (a PC), it gives each the standard
  partition type for its role, records each one's unique partition ID in the installation
  record, and writes the system partition's and userdata's IDs into the boot entry's
  command line as the identity keys of [boot 001](https://github.com/GlassineOS/boot/blob/main/specs/001-deployments/spec.md)
  (FR-020). No disk goes into a base's or a deployment's file system table. Where a
  device's partitions have fixed names, its description names them instead and no keys
  are written. The recovery rebuilds a lost boot entry from the installation record.
- **FR-013**: The installation is complete only when the device has booted the first
  deployment and it has been accepted (boot 001). Until then, the installer shows the
  installation as unfinished.
- **FR-014**: The installation record (on the device and the computer) holds the device's
  identity, the description's version, the installer's version, the resulting layout, the
  first deployment, the capabilities, and the backup's location.

### Devices and backends

- **FR-015**: The flow, its screens and its checks are the same for every device. What
  differs comes from the description and the backend.
- **FR-016**: A device lacking a capability the system needs to boot safely is refused
  with the reason. One lacking an optional capability is installed without it, as the plan
  says.
- **FR-017**: The installer runs from Linux, Windows or a browser on a computer connected
  to the device, and on a PC from removable media started on that PC.
- **FR-018**: The installer reaches a device through the connections its description
  names, such as its boot loader's own USB protocol or a mode of its stock system. Where a
  boot loader departs from its protocol's standard, the description says how, and the
  installer works with the boot loader as it shipped. Installing never needs the boot
  loader changed first.
- **FR-019**: Inspecting reports which of the device's connections the installer can use
  from where it runs (a browser can use fewer than a program on the computer). With none,
  the report says so and where the installation can run instead. The plan uses only
  connections the running installer has.

## Key entities

- **Device description**: how to recognise a device, how the installer reaches it
  (FR-018), its capabilities, its partitions
  (never-write, back up, replace, create), its device data, its unit-specific values, its
  backend, the texts it shows.
- **Backend**: the steps for one platform family (devices that start from an Android
  bootloader; PCs that start from UEFI).
- **Plan**: everything an installation will do, with each step's escape.
- **Backup**: one full image of each disk the installation writes to, and the captured
  values, stored off those disks (FR-009).
- **Installation record**: what was installed, on what, with which versions.

## Success criteria

- **SC-001**: People who have never used a command line install GlassineOS on a supported
  device from its stock state using only the installer. In a test with at least five such
  people, at least four finish without help.
- **SC-002**: Inspecting a machine leaves every partition byte-identical, verified by
  comparing the machine before and after.
- **SC-003**: With an interruption injected after every step of the plan, every run ends
  in one of the three states of Scenario 4.
- **SC-004**: Going back to stock restores every partition byte-identical to the full
  backup.
- **SC-005**: The Note Air and a UEFI PC are installed by the same flow. Neither has a
  screen or check the other lacks, apart from the texts of their descriptions.

## Assumptions

- Bases come as the send streams of 0013. Where a base comes from (built on the computer or
  downloaded) is not this feature's concern.
- First-boot setup runs after the accepted boot and isn't part of the installer.
- The GlassineOS recovery is the same on every machine (0014); on a machine that keeps
  another system, that system's own recovery stays as the secondary one it hands over to.
