# Feature Specification: The device description

**Feature branch:** `device-description`
**Created:** 2026-11-12 · **Status:** spec, for review; nothing built
**Decision:** [docs 0014](https://docs.glassine.org/decisions/0014-installation-and-provisioning/),
[docs 0015](https://docs.glassine.org/decisions/0015-installer-stack/) · **Builds on:**
[001](../001-install-from-stock/spec.md) · **Contract:** owned by this repository, format 2

## Summary

Everything the installer knows about one kind of machine is in its device description: how
to recognise it, which install state it is in, how the installer reaches it, what it may
and may not write, what it backs up, what an installation does, step by step, and how each
step is escaped. The flow (001) reads descriptions and never names a device.

A description is data. It says *what* to do, with parameters; the backend of the device's
platform family knows *how*. A description holds no commands or scripts. A device that
needs a step no backend has gets that step added to its backend, not written into its
description.

Descriptions belong to the device's own repository. The installer ships copies of them,
each pinned to the version it was checked against, and the installed system carries its
own device's description too. A description is the only place a device's facts are
written: the installed system's tools read them from it, and nothing else restates them.
What runs on the device (its services and bring-up) is code and lives in the device's
package; how its system is built lives in the image build; neither restates a fact the
description holds.

This format is an interface other repositories write to, so this spec defines it exactly
and versions it.

## User scenarios

### Scenario 1 – Add a device (P1)

**Given** a device whose platform family has a backend,
**When** someone writes its description in the device's repository and checks it,
**Then** the check either accepts it or names every problem with the key it is at, and an
accepted description makes the installer recognise and plan for the device with no change
to the installer's code.

### Scenario 2 – A description from a newer installer (P1)

**Given** a description in a newer format than the installer reads,
**When** the installer loads it,
**Then** it refuses that description by name and says the installer is too old, and goes on
with the others. It never reads a newer description partly.

### Scenario 3 – Recognise without guessing (P1)

**Given** a machine whose signals match one description's identity,
**When** it is inspected,
**Then** it is recognised as that device, and its install state is the one state whose
conditions observable in the machine's mode hold; when that mode can't tell the state, the
installer names the mode that can.
**And given** a machine in a mode where some of a description's required signals can't be
seen, and none of those that can be seen contradicts, **then** the installer says which
mode would decide and moves there or asks, never guessing a match.
**And given** signals of which some match and some contradict a description, **then** the
machine is treated as unsupported and the report names the conflicting signals.
**And given** signals that match no description, **then** the machine is unsupported, and
the report lists what was seen.

### Scenario 4 – Untried steps are visible (P1)

**Given** a description with a step or a signal that hasn't been tried on real hardware,
**When** the person sees the plan,
**Then** the plan says which, and the installer doesn't run an untried write step for
anyone but a developer who asked for it.

### Scenario 5 – A pinned copy that drifted (P2)

**Given** a copy of a description in the installer that differs from the version it is
pinned to,
**When** the installer's checks run,
**Then** they fail and name the description.

## The format

### File and versions

- **FR-001**: A description is one TOML file. Its first key is `format`; this spec is
  `format = 2`. The format includes its vocabularies: the fact keys signals read, the mode
  kinds, step kinds, boot-setting kinds and capability ids, each with what its parameters
  are.
- **FR-002**: A reader rejects a description with an unknown key, a missing required key, a
  wrong type, or a `format` it doesn't know. It reports every problem at once, each with the
  key's full path.
- **FR-003**: Any change to the format raises `format`: a new key, a new value, a new entry
  in a vocabulary. A reader reads its own format and every earlier one, and refuses a later
  one as in Scenario 2, never as invalid. The formats:

  | Format | Differs from the one after it |
  |--------|-------------------------------|
  | 1      | No `disk` role (FR-016a): each `back-up` and `replace` rule names its own backup, and no job must image whole disks. The classes `read` and `back-up` exist and mean `never-write`. A new partition has no `at_least`, and a layout's partitions have no sizes (FR-014a). |
  | 2      | This spec. |
- **FR-004**: The format, vocabularies included, is also published as a machine-readable
  schema for each `format`, so editors and the device repositories' own checks validate a
  description completely without the installer. The schema names every key of the format;
  this spec names the keys whose meaning it needs to state.

### Who the device is

- **FR-005**: `id` (lowercase, `-` between words, unique among descriptions), `name` (shown
  to people), `backend` (the platform family: `android-bootloader` or `uefi-pc` today).
- **FR-006**: `[[identity]]` lists signals, each with the fact it reads (`key`), a test
  (`equals`, `one-of`, `present`, `absent`, `at-least`, `at-most`), whether it is
  `required` or supporting, and whether it has been seen on real hardware (`verified`, with
  a note when not). The fact key's vocabulary entry names the mode kinds the fact can be
  observed in, so a signal needs no mode of its own: identity is the same in every mode.
- **FR-007**: A description is matched when all its required signals hold. When some
  required signals hold and another contradicts, the result is a conflict (Scenario 3).
  Supporting signals only add confidence and never make a match on their own. A signal
  whose fact can't be observed in the machine's mode is undetermined, never a
  contradiction: when some required signals hold, none contradicts and the rest are
  undetermined, recognition is undetermined, and the report names the description and the
  modes that would decide (the device's modes whose kind can observe the missing facts).
- **FR-007a**: A description may name one more general description it `refines` (a model of
  a family, a particular PC of the generic PC). When several match, the one that refines the
  others wins. Two matches neither of which refines the other are ambiguous: the machine is
  treated as unsupported and the report names both.
- **FR-007b**: A description that refines another and half-matches (some of its required
  signals hold, another contradicts) while the one it refines matches is not a conflict:
  the machine is that more general device, not that model. Every other half-match is a
  conflict (FR-007).
- **FR-008**: Identity uses what the hardware is, never what a person named it and never
  what install state it is in. A device's description has at least two required signals of
  different kinds (for example, the storage's layout and the boot loader's platform). A
  generic description (`generic = true`, such as the UEFI PC) matches on its platform alone
  and is refined by any description that matches too. A signal that changes
  when GlassineOS is installed (a boot loader's product string, a partition the
  installation creates) belongs to a state, not to the identity.
- **FR-009**: `[[states]]` lists the install states in order (the first is the device as its
  maker ships it), each with an `id` and the conditions that tell it (same tests as
  FR-006). Every condition, here and in a step's `done` (FR-022), names the mode it can be
  observed in. A condition is true, false, or undetermined in the mode the device is in.
  A state is judged on its conditions that are observable in that mode: it holds when they
  all hold, and it is undetermined when it has none there. When exactly one state holds,
  the state is decided. When several hold and each of them also has conditions in other
  modes, the state is undetermined, not a conflict; an undetermined state says which modes
  would decide it, and the installer moves there (FR-013) or asks. Several states that hold
  on conditions all observable in the one mode are a conflict. The standard ids are
  `stock`, `prepared` (changed by an earlier installation's first steps, nothing of
  GlassineOS booted yet) and `installed`; a description may add others.
- **FR-009a**: A half-installed device is not a state a description lists: it is a device
  some of whose job's steps are done and some not, known from the steps' `done` conditions
  and the installation record (001 FR-008).
- **FR-009b**: `installer` is a reserved mode, never a device's: it means the installer's
  own records on the computer (a verified backup, a step's write that the installer read
  back). A condition names it when the device can't show the result, as with a backup or a
  raw image write. No description lists a mode with that id.

### How the installer reaches it

- **FR-010**: `[[modes]]` lists the device's ways of talking to a computer, or, for a PC, the
  installer running on it. Each mode has an `id`, a `kind` (the boot loader's protocol, the
  stock system's, the device's emergency mode, a GlassineOS one, or `local`), how it shows
  on USB, and what it can do: `identify`, `read` (partitions or files), `write`, and the
  modes it can restart into.
- **FR-011**: Where a mode's protocol departs from its standard, the mode says how, as data
  its backend understands (for example, that every transfer must have one length, and which
  length). A departure that must be known before the device is recognised names the first
  answer that tells it (FR-012). An operation can be one never to send at all, or one
  allowed only after a check of the device's state that shows it changes nothing (for
  example, booting an image only when the active slot's flags show it writes none of them);
  in any other state it needs the person's acceptance.
- **FR-012**: A backend reaches an unrecognised device with an opening that is safe whether
  or not a departure applies. It chooses how to go on from the first answer, using the
  departures of every description it has, before matching any.
- **FR-013**: `[[transitions]]` lists how the device gets from one mode to another: whether
  the installer can do it (through a mode that can restart into the other) or the person
  does it by hand (with the text that guides them), and what it needs (a setting on the
  device, special hardware such as an emergency cable, the person's hands).

### What it may write

- **FR-014**: `[[partitions]]` is an ordered list of rules. Each selects partitions (by
  name, a name pattern, a partition type, or a role such as "the EFI system partition") and
  gives its class, the most any job may do to them. The first rule that selects a partition
  decides. A partition no rule selects is `never-write` (001 FR-004). The classes are not
  one order; each allows exactly these actions of a step:

  | Class         | reads | backs up | writes | creates | resizes | removes |
  |---------------|:-----:|:--------:|:------:|:-------:|:-------:|:-------:|
  | `never-write` |  yes  |   yes    |        |         |         |         |
  | `replace`     |  yes  |   yes    |  yes   |         |         |         |
  | `create`      |       |          |  yes   |   yes   |         |         |
  | `resize`      |  yes  |   yes    |  yes   |         |   yes   |         |
  | `remove`      |  yes  |   yes    |  yes   |         |   yes   |   yes   |

  A `create` partition doesn't exist before the job, so there is nothing of it to read or
  back up.
- **FR-014a**: What a plan does to each partition comes from its steps: each step names the
  partitions it reads, writes, creates, resizes or removes. The plan's list (001 FR-003)
  is derived from the steps the choices select, and the check rejects a step that would go
  beyond its partition's rule. A size may be relative (the rest of a disk, the free space,
  what is left after another partition), and a relative size may name the least it can be
  (`at_least`): a fixed size, or `release:<role>`, the least the installer's release
  states for that artifact role (FR-019a). What GlassineOS itself needs is the release's
  to state, so a system partition's least is `release:base`; a fixed `at_least` is for
  what the release doesn't know. A partition of fixed size may name `release:<role>` as
  well; a step that writes a partition table from its layout lists, with their fixed
  sizes, the partitions of the layout whose size the plan checks. A plan is refused, with
  the space it lacks, when its disk can't give the new partitions their fixed sizes and
  those least sizes, or when a fixed partition is smaller than the least its release
  states (the size inspecting found, where it found the partition, else the
  description's). A least the release doesn't state (there is no release, as in a dry run
  or a test, or one that doesn't state it) is not known yet: the plan says so, counts the
  disk's space as at least what is known, and never refuses for it.
- **FR-014b**: Renaming a partition is not an action: it is a parameter of the step that
  writes the partition table (old name = new name), and that step writes the table. The
  rules select a renamed partition by both its names.
- **FR-015**: A `resize` or `remove` says whether it destroys what's on the partition. The plan
  shows a destroying change as the loss of the person's files, never as a technical step.
- **FR-016**: A change to how the machine starts (a boot slot's flags, a firmware boot entry,
  the boot loader itself) is its own planned object, of a kind the backend defines, never
  hidden in a partition write.

### What it keeps

- **FR-016a**: The backup is the whole disk (001 FR-009). A rule selects the role `disk`
  (a disk the job writes to, as one object: its partition table and every partition) and
  its `back_up` names how the disk is read (the mode and its means), how the image is
  checked (read back and compared), and the image's size as stored (compressed) when it is
  known. Every job that writes has a `back-up` step whose `backs_up` is `role:disk`, and
  every step that writes comes after it. The plan resolves `role:disk` to each disk its
  steps write to and to no other. A boot setting of FR-016 that lives on the disk is backed
  up `with` it; one that doesn't (a firmware variable) names its own `back_up` the same
  way. The plan's space (001 FR-003) counts the backup as well as the artifacts.
- **FR-016b**: Whether the backup is made, and where it goes, is the flow's choice, the
  same on every machine (001 FR-009a): the choices `backup` (`full`, the default, or `off`)
  and `backup-to`. No description has a choice with these ids, and none switches the
  backup off.

- **FR-017**: `[[unit_values]]` lists the values unique to the unit that GlassineOS can't read
  itself: what each is, the mode and means of reading it, and where in the installed system
  it goes. They are read before the first write and never leave the person's computer.
- **FR-018**: `[[device_data]]` lists the device-data files the installed system needs
  ([boot 006](https://github.com/GlassineOS/boot/blob/main/specs/006-device-data-storage-access/spec.md)):
  for each, the name the system uses, the partition and path it is read from, and how it
  is checked, as complete as boot 006's list needs. The device-data list the installed
  system reads is derived from these entries and from nothing else.
- **FR-018a**: `[[device_files]]` lists the vendor files the installation needs from the
  device itself ([0008](https://docs.glassine.org/decisions/0008-bring-your-own-stock-kernel/)):
  what each is, where on the device it is read, and what uses it. They stay on the person's
  computer for the installation only (001 FR-011).

### What an installation does

- **FR-019**: `[[artifacts]]` lists what the installation writes that doesn't come from the
  device, by role (the base, the kernel, the boot loader, the recovery, the partition
  layout), never by version or address: the installer's release supplies each role and
  checks its integrity. A size is marked unknown until the release supplies it.
- **FR-019a**: The installer's release has a manifest, a JSON object whose
  `release_format` is 1, that states per artifact role what the plan reads:
  `artifacts.<role>.min_partition_bytes`, the least size in bytes of the partition that
  role's artifact goes to. A reader ignores keys it doesn't know and refuses a later
  `release_format`. The installer only reads the number; how it is worked out is the
  release's rule, owned by the release side (GlassineOS/images, which builds the base and
  records it in its artifacts folder, produces it with each release; the installer never
  computes it). For `base` the rule is: the system partition holds the base snapshots
  on btrfs, which share most of their blocks, and the kernel modules, so its least is one
  base's size as received on btrfs (its used bytes, not the compressed send stream), plus
  64 MiB for each of 4 kept updates, plus the kernel modules' size, all × 1.25 for the
  file system's metadata and slack; at least 1 GiB, rounded up to a whole MiB. The
  person's home and Flatpak data are on the data partition and don't count.
- **FR-020**: `[[capabilities]]` states each capability the installed device has (choose a
  kernel, a one-shot trial boot, a recovery, keep the stock system, ...): `have`, `lack`, or
  `needs` (with what), each with `verified`. The ids are shared by all descriptions.
- **FR-021**: `[[choices]]` lists what the person decides in the plan, with a default: keep
  the stock system, the target disk. A choice can be a template repeated for each item
  inspecting finds (what happens to each operating system found). Replacing an operating
  system is never a default. A person chooses outcomes, never partitions.
- **FR-022**: `[[steps]]` lists the installation's steps. Each has a stable `id`, a `kind`
  from its backend's list of step kinds, the kind's parameters, the mode it needs, the steps
  it comes after (the order; the list's own order means nothing), the choices it depends
  on, and a `done` condition (so an interrupted installation knows where it stands). The
  installer observes it on the device or, for a step whose result the device can't show,
  in its own record (FR-009b).
- **FR-023**: Each step says how it is escaped, separately for a failure during it and for an
  interruption after it: the state that leaves the device in (its previous system starts,
  GlassineOS starts, the installer can finish, the installer can undo, or only special
  hardware helps, and which) and the way out (another step, or a transition). It also says
  whether it is a point of no return, whether a cancel may stop right after it (a safe
  stop, 001's edge cases), and whether it has been tried on real hardware. A step whose way
  out needs special hardware makes the plan ask for it first (001).
- **FR-023a**: `verified` (here and in FR-006 and FR-020) points to a recorded run on real
  hardware and says the state and boot loader it was made on. Without such a record, it is
  untried.
- **FR-024**: The same structure describes the other jobs of 001: repair, reinstall and back
  to stock are lists of steps of their own, chosen by the device's state, and back to stock
  restores what FR-016a backed up.
- **FR-024a**: What every job does on every machine is the flow's, never a description's:
  the confirmation, the installation record, and waiting for the first boot to be accepted
  (001 FR-013).
- **FR-025**: A description that names something its format's vocabularies don't have, or
  that its backend doesn't implement, is rejected by the check.

### Texts

- **FR-026**: `[texts]` holds what the installer shows for this device: the name, how to
  reach each mode by hand, what each choice means here. Plain text, no markup, English
  first; keys the flow asks for are listed by the flow.

### Pinned copies

- **FR-027**: A description lives in its device's repository. The installer ships a copy
  that is byte-identical to one version of it, and a lock file lists, per copy, the
  repository, the version and the content's hash.
- **FR-028**: The installer's checks fail when a copy differs from its lock entry, when a
  lock entry has no copy, or when a copy fails FR-002.

### On the installed system

- **FR-029**: The installed system holds its device's description at
  `/usr/share/glassine/device.toml`, byte-identical to one version in the device's
  repository and installed with the device's package.
- **FR-030**: Every file on the installed system that states a device fact the
  description holds (the device-data list of FR-018; where the description keeps another
  system, the slot that system starts from) is derived from the description when the
  device's package is built, and the build fails when a derived file and the description
  disagree. No such file is written by hand.

## What a description never holds

- Commands, scripts or code of any kind. The device's runtime code (services, bring-up of
  its radios and sound) is in its package.
- Vendor files, or hashes or excerpts of them; values unique to one unit (serial numbers,
  addresses, calibration); anything learned from one person's device.
- Markup in texts.

## Key entities

- **Description**: one kind of machine, in format 2.
- **Signal**: one test of one fact, required or supporting, verified or not.
- **State**: an install state and the conditions that tell it, each in the mode it is
  observable in.
- **Mode**: a way the installer talks to the device, with its departures from the standard.
- **Transition**: how the device gets from one mode to another, and what that needs.
- **Partition rule**: a selection and the most any job may do; unselected means never
  written. The rule for the role `disk` says how the whole-disk backup is read.
- **Step**: a kind from the backend, its parameters, its `done` condition and its escape.
- **Release manifest**: what the installer's release states per artifact role, such as
  the least size of the partition the base goes to.
- **Lock entry**: which version of a device repository's description the installer ships.
- **Installed description**: the copy on the installed system, and the files derived from
  it for the system's own tools.

## Success criteria

- **SC-001**: The descriptions of the Note Air and of a UEFI PC are both written in format 2
  and accepted, and the same installer recognises and plans for both with no device named
  in its code.
- **SC-002**: For every rule of FR-002 and FR-025 there is a description that breaks it, and
  the check rejects it with the key's path.
- **SC-003**: Every step in both descriptions has a `done` condition and an escape, and for
  each step a test interrupts the installation right after it and reaches one of 001's
  three end states using only what the description says.
- **SC-004**: Changing one byte of a shipped copy fails the checks (Scenario 5).
- **SC-005**: On an installed Note Air, the device-data list and the slot of the other
  system match what its installed description says, and editing either derived file by
  hand in the package's sources fails the package's build.

## Assumptions

- A backend implements the step kinds, mode kinds and boot-setting kinds of the formats it
  reads; a vocabulary entry no backend implements yet can exist in a format.
- A description can be written before every step of it has been tried; FR-006 and FR-023
  keep that visible.
