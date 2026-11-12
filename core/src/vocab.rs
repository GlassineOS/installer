// SPDX-License-Identifier: GPL-2.0-or-later
//! The vocabularies of format 1: every name a description may use, with what
//! its parameters are. They are data, so the validator, the schema and the
//! planner read the same lists. A new entry is a format change (FR-003).

/// The type of a fact's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactType {
    /// A string, compared with `equals` / `one-of`.
    Text,
    /// A whole number (bytes for sizes), compared with `at-least` / `at-most` too.
    Number,
    /// Only `present` / `absent` make sense.
    Presence,
}

/// What a fact is about. FR-008 asks for required identity signals of at
/// least two different kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FactKind {
    Usb,
    BootLoader,
    System,
    Storage,
    Firmware,
    Cpu,
    Board,
    Glassine,
    Installer,
}

impl FactKind {
    pub fn name(self) -> &'static str {
        match self {
            FactKind::Usb => "usb",
            FactKind::BootLoader => "boot-loader",
            FactKind::System => "system",
            FactKind::Storage => "storage",
            FactKind::Firmware => "firmware",
            FactKind::Cpu => "cpu",
            FactKind::Board => "board",
            FactKind::Glassine => "glassine",
            FactKind::Installer => "installer",
        }
    }
}

/// One fact key, or a family of them: `<name>` in `pattern` stands for one
/// path segment the description fills in (a variable, a partition name).
#[derive(Debug)]
pub struct FactKey {
    pub pattern: &'static str,
    pub ty: FactType,
    pub kind: FactKind,
    /// The mode kinds it can be observed in (`installer` = the installer's own
    /// records on the computer).
    pub modes: &'static [&'static str],
    /// A value unique to one unit: never in a description, never in a report.
    pub unit_unique: bool,
    pub doc: &'static str,
}

pub const ANY_DEVICE_USB: &[&str] = &["fastboot", "android-adb", "emergency-download", "glassine-usb"];
const STORAGE_MODES: &[&str] = &["fastboot", "android-adb", "emergency-download", "local"];

pub const FACT_KEYS: &[FactKey] = &[
    FactKey {
        pattern: "usb.vendor-id",
        ty: FactType::Text,
        kind: FactKind::Usb,
        modes: ANY_DEVICE_USB,
        unit_unique: false,
        doc: "USB vendor id, four lower-case hex digits",
    },
    FactKey {
        pattern: "usb.product-id",
        ty: FactType::Text,
        kind: FactKind::Usb,
        modes: ANY_DEVICE_USB,
        unit_unique: false,
        doc: "USB product id, four lower-case hex digits",
    },
    FactKey {
        pattern: "usb.interface.<name>",
        ty: FactType::Presence,
        kind: FactKind::Usb,
        modes: ANY_DEVICE_USB,
        unit_unique: false,
        doc: "an interface with class-subclass-protocol <name> (hex, e.g. ff-42-03)",
    },
    FactKey {
        pattern: "usb.serial",
        ty: FactType::Text,
        kind: FactKind::Usb,
        modes: ANY_DEVICE_USB,
        unit_unique: true,
        doc: "the USB serial number (unit-unique; never in a description)",
    },
    FactKey {
        pattern: "fastboot.var.<name>",
        ty: FactType::Text,
        kind: FactKind::BootLoader,
        modes: &["fastboot"],
        unit_unique: false,
        doc: "the boot loader's answer to getvar:<name>; absent when it answers FAIL",
    },
    FactKey {
        pattern: "android.prop.<name>",
        ty: FactType::Text,
        kind: FactKind::System,
        modes: &["android-adb"],
        unit_unique: false,
        doc: "a system property of the running stock Android",
    },
    FactKey {
        pattern: "storage.partition.<name>",
        ty: FactType::Presence,
        kind: FactKind::Storage,
        modes: STORAGE_MODES,
        unit_unique: false,
        doc: "a partition named <name> exists",
    },
    FactKey {
        pattern: "storage.partition.<name>.size",
        ty: FactType::Number,
        kind: FactKind::Storage,
        modes: STORAGE_MODES,
        unit_unique: false,
        doc: "the size of partition <name> in bytes",
    },
    FactKey {
        pattern: "storage.kind",
        ty: FactType::Text,
        kind: FactKind::Storage,
        modes: &["local"],
        unit_unique: false,
        doc: "the kind of the first disk: emmc, ufs, nvme, sata, usb, other",
    },
    FactKey {
        pattern: "firmware.kind",
        ty: FactType::Text,
        kind: FactKind::Firmware,
        modes: &["local"],
        unit_unique: false,
        doc: "how the machine starts: uefi or bios",
    },
    FactKey {
        pattern: "firmware.secure-boot",
        ty: FactType::Text,
        kind: FactKind::Firmware,
        modes: &["local"],
        unit_unique: false,
        doc: "UEFI secure boot: on, off or setup",
    },
    FactKey {
        pattern: "cpu.arch",
        ty: FactType::Text,
        kind: FactKind::Cpu,
        modes: &["local"],
        unit_unique: false,
        doc: "the processor architecture: x86-64 or arm64",
    },
    FactKey {
        pattern: "cpu.vendor",
        ty: FactType::Text,
        kind: FactKind::Cpu,
        modes: &["local"],
        unit_unique: false,
        doc: "the processor's maker as the processor reports it",
    },
    FactKey {
        pattern: "board.vendor",
        ty: FactType::Text,
        kind: FactKind::Board,
        modes: &["local"],
        unit_unique: false,
        doc: "the system's maker (SMBIOS)",
    },
    FactKey {
        pattern: "board.product",
        ty: FactType::Text,
        kind: FactKind::Board,
        modes: &["local"],
        unit_unique: false,
        doc: "the system's product name (SMBIOS)",
    },
    FactKey {
        pattern: "board.serial",
        ty: FactType::Text,
        kind: FactKind::Board,
        modes: &["local"],
        unit_unique: true,
        doc: "the system's serial number (unit-unique; never in a description)",
    },
    FactKey {
        pattern: "board.uuid",
        ty: FactType::Text,
        kind: FactKind::Board,
        modes: &["local"],
        unit_unique: true,
        doc: "the system's UUID (unit-unique; never in a description)",
    },
    FactKey {
        pattern: "glassine.installation",
        ty: FactType::Presence,
        kind: FactKind::Glassine,
        modes: &["glassine-usb", "local"],
        unit_unique: false,
        doc: "a GlassineOS installation record on the machine",
    },
    FactKey {
        pattern: "installer.backup-verified",
        ty: FactType::Presence,
        kind: FactKind::Installer,
        modes: &["installer"],
        unit_unique: false,
        doc: "this job's backup is on the computer and verified",
    },
    FactKey {
        pattern: "installer.step.<name>",
        ty: FactType::Presence,
        kind: FactKind::Installer,
        modes: &["installer"],
        unit_unique: false,
        doc: "the installer's record says step <name> completed",
    },
];

/// Words that mark a key or parameter as unit-unique even when no pattern
/// says so (`fastboot.var.serialno`, `android.prop.ro.serialno`, ...).
pub const UNIT_UNIQUE_WORDS: &[&str] = &[
    "serial",
    "uuid",
    "guid",
    "imei",
    "meid",
    "mac",
    "hostname",
    "wifi-addr",
    "bt-addr",
    "btaddr",
    "macaddr",
];

/// The reserved mode id for conditions the installer observes in its own
/// records on the computer (a backup's check, a step it completed).
pub const INSTALLER_MODE: &str = "installer";

/// Finds the fact key a concrete key belongs to and the parameter it fills.
pub fn fact_key(key: &str) -> Option<(&'static FactKey, Option<String>)> {
    for fk in FACT_KEYS {
        match fk.pattern.find("<name>") {
            None if fk.pattern == key => return Some((fk, None)),
            None => {}
            Some(at) => {
                let (pre, post) = (&fk.pattern[..at], &fk.pattern[at + 6..]);
                if key.len() > pre.len() + post.len() && key.starts_with(pre) && key.ends_with(post) {
                    let param = &key[pre.len()..key.len() - post.len()];
                    // A parameter is one segment: `storage.partition.x.size` must not read as
                    // `storage.partition.<x.size>`.
                    if !param.is_empty() && !(post.is_empty() && fk.pattern.starts_with("storage.partition.") && param.contains('.')) {
                        return Some((fk, Some(param.to_string())));
                    }
                }
            }
        }
    }
    None
}

/// True when a key, or the parameter it fills, names a value unique to one unit.
pub fn looks_unit_unique(key: &str) -> bool {
    if let Some((fk, _)) = fact_key(key)
        && fk.unit_unique
    {
        return true;
    }
    let k = key.to_ascii_lowercase();
    k.split(['.', ':', '_', '-']).any(|seg| UNIT_UNIQUE_WORDS.contains(&seg)) || UNIT_UNIQUE_WORDS.iter().any(|w| w.len() > 4 && k.contains(w))
}

/// A named entry with a short meaning, for the plain vocabularies.
#[derive(Debug)]
pub struct Entry {
    pub id: &'static str,
    pub doc: &'static str,
}

pub const fn e(id: &'static str, doc: &'static str) -> Entry {
    Entry { id, doc }
}

pub fn ids(list: &[Entry]) -> Vec<&'static str> {
    list.iter().map(|e| e.id).collect()
}

pub const BACKENDS: &[Entry] = &[
    e("android-bootloader", "devices that start from an Android boot loader"),
    e("uefi-pc", "PCs that start from UEFI"),
];

pub const CONDITION_TESTS: &[Entry] = &[
    e("equals", "the fact has this value"),
    e("one-of", "the fact has one of these values"),
    e("present", "the fact exists (true)"),
    e("absent", "the fact does not exist (true)"),
    e("at-least", "the number is at least this"),
    e("at-most", "the number is at most this"),
];

/// A parameter of a vocabulary entry.
#[derive(Debug)]
pub struct Param {
    pub name: &'static str,
    pub ty: ParamType,
    pub required: bool,
    pub doc: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    Text,
    Number,
    Bool,
    /// A size: bytes, or relative (FR-014a).
    Size,
    /// An artifact role.
    Artifact,
    /// A partition reference (a name, or `role:<role>`).
    Partition,
    /// A boot slot letter.
    Slot,
    /// A table of partition renames, old name = new name.
    Renames,
    /// A list of partitions to create, each `{ name, size, role? }`.
    NewPartitions,
    /// An operation of the mode kind.
    Operation,
}

pub const fn p(name: &'static str, ty: ParamType, required: bool, doc: &'static str) -> Param {
    Param { name, ty, required, doc }
}

/// A mode kind with what it can do and the protocol operations a departure
/// can name.
#[derive(Debug)]
pub struct ModeKind {
    pub id: &'static str,
    pub doc: &'static str,
    pub backends: &'static [&'static str],
    pub operations: &'static [&'static str],
}

pub const MODE_KINDS: &[ModeKind] = &[
    ModeKind {
        id: "fastboot",
        doc: "the boot loader's fastboot protocol over USB",
        backends: &["android-bootloader"],
        operations: &["getvar", "download", "flash", "erase", "boot-image", "reboot", "set-active", "fetch"],
    },
    ModeKind {
        id: "android-adb",
        doc: "the stock Android system's adb over USB",
        backends: &["android-bootloader"],
        operations: &["getprop", "reboot", "reboot-bootloader", "reboot-recovery", "reboot-edl"],
    },
    ModeKind {
        id: "emergency-download",
        doc: "the SoC's emergency download mode (Qualcomm EDL); special hardware or a software route",
        backends: &["android-bootloader"],
        operations: &["read", "write", "erase", "reset"],
    },
    ModeKind {
        id: "glassine-usb",
        doc: "a running GlassineOS or its recovery, over USB (not built yet)",
        backends: &["android-bootloader"],
        operations: &[],
    },
    ModeKind {
        id: "local",
        doc: "the installer running on the machine itself (a PC from removable media)",
        backends: &["uefi-pc"],
        operations: &["read", "write", "efi-variables"],
    },
];

pub fn mode_kind(id: &str) -> Option<&'static ModeKind> {
    MODE_KINDS.iter().find(|m| m.id == id)
}

pub const MODE_CAN: &[Entry] = &[
    e("identify", "answers enough to recognise the device"),
    e("read", "reads partitions or files"),
    e("write", "writes partitions or files"),
];

/// A departure from a mode's standard protocol (FR-011).
#[derive(Debug)]
pub struct Kind {
    pub id: &'static str,
    pub doc: &'static str,
    pub backends: &'static [&'static str],
    /// For departures and backup means: the mode kinds it applies to. For step
    /// kinds: the mode kinds a step of this kind can run in.
    pub modes: &'static [&'static str],
    pub params: &'static [Param],
}

use ParamType as T;

pub const DEPARTURES: &[Kind] = &[
    Kind {
        id: "equal-length-transfers",
        doc: "after a warm-up, every transfer must be exactly `length` bytes: commands padded with NUL, data in chunks of `length`, a download rounded up to whole chunks with zeros",
        backends: &["android-bootloader"],
        modes: &["fastboot"],
        params: &[
            p("length", T::Number, true, "the one transfer length; not a multiple of the USB packet size"),
            p("warm-up", T::Text, true, "the opening that arms it: short-command-then-dummy"),
        ],
    },
    Kind {
        id: "fresh-boot-only",
        doc: "the protocol only works in a fresh boot of the mode; a session that finds it used restarts the mode",
        backends: &["android-bootloader"],
        modes: &["fastboot"],
        params: &[],
    },
    Kind {
        id: "never-use",
        doc: "an operation of the protocol that must never be sent",
        backends: &["android-bootloader", "uefi-pc"],
        modes: &["fastboot", "android-adb", "emergency-download", "local"],
        params: &[
            p("operation", T::Operation, true, "the operation"),
            p("reason", T::Text, true, "why, in plain words"),
        ],
    },
    Kind {
        id: "single-variables-only",
        doc: "variables are read one at a time; the protocol's all-variables request is not used",
        backends: &["android-bootloader"],
        modes: &["fastboot"],
        params: &[],
    },
];

pub const NEEDS: &[Entry] = &[
    e("hands", "the person does it by hand"),
    e("usb-debugging", "a setting on the device: USB debugging on in the stock system"),
    e("cover", "the device's magnetic cover"),
    e("emergency-cable", "special hardware: a USB cable that forces the emergency download mode"),
    e("removable-media", "the installer on a USB stick or other removable media"),
    e("firmware-setup", "a setting in the PC's firmware setup"),
];

/// Needs that are special hardware (001: the plan asks for it first).
pub const SPECIAL_HARDWARE: &[&str] = &["emergency-cable"];

/// The partition classes: the most any job may do to a partition (FR-014).
pub const CLASSES: &[Entry] = &[
    e("never-write", "never written; read only to inspect it"),
    e("read", "steps may read it"),
    e("back-up", "steps may read it and back it up"),
    e("replace", "steps may back it up and write it"),
    e("create", "a partition the job creates and then writes"),
    e("resize", "steps may change its size (and write it)"),
    e("remove", "steps may remove it (and anything a resize may do)"),
];

/// What a step does to a partition.
pub const ACTIONS: &[Entry] = &[
    e("reads", "reads it"),
    e("backs_up", "backs it up to the computer"),
    e("writes", "writes its content"),
    e("creates", "creates it"),
    e("resizes", "changes its size"),
    e("removes", "removes it"),
];

/// The allowed-set table: which step actions each class permits. The classes
/// are not one order: `create` permits writing what it created, never reading
/// or backing up a partition that existed before.
pub fn class_allows(class: &str, action: &str) -> bool {
    let allowed: &[&str] = match class {
        "never-write" => &[],
        "read" => &["reads"],
        "back-up" => &["reads", "backs_up"],
        "replace" => &["reads", "backs_up", "writes"],
        "create" => &["creates", "writes"],
        "resize" => &["reads", "backs_up", "writes", "resizes"],
        "remove" => &["reads", "backs_up", "writes", "resizes", "removes"],
        _ => &[],
    };
    allowed.contains(&action)
}

pub const ROLES: &[Entry] = &[
    e("partition-table", "the disk's partition table, as one object"),
    e("esp", "an EFI system partition"),
    e("os", "a partition that belongs to an operating system inspecting found"),
    e("free-space", "unpartitioned space on the target disk"),
];

pub const SELECTORS: &[Entry] = &[
    e("name", "one partition by name"),
    e("names", "several partitions by name"),
    e("pattern", "names matching a pattern; `*` stands for any run of characters"),
    e("type", "a partition type (a GPT type GUID)"),
    e("role", "a role (see the roles)"),
];

pub const BACKUP_MEANS: &[Kind] = &[
    Kind {
        id: "fastboot-fetch",
        doc: "the boot loader sends a partition's content back (fastboot fetch)",
        backends: &["android-bootloader"],
        modes: &["fastboot"],
        params: &[],
    },
    Kind {
        id: "fastboot-getvar",
        doc: "values the boot loader answers (slot flags, partition sizes)",
        backends: &["android-bootloader"],
        modes: &["fastboot"],
        params: &[],
    },
    Kind {
        id: "emergency-read",
        doc: "read over the emergency download mode",
        backends: &["android-bootloader"],
        modes: &["emergency-download"],
        params: &[],
    },
    Kind {
        id: "block-read",
        doc: "read the block device on the machine itself",
        backends: &["uefi-pc"],
        modes: &["local"],
        params: &[],
    },
    Kind {
        id: "efi-variables",
        doc: "read the firmware's boot variables",
        backends: &["uefi-pc"],
        modes: &["local"],
        params: &[],
    },
];

pub const BACKUP_CHECKS: &[Entry] = &[e(
    "read-back-compare",
    "read back from the computer and compared with what was read from the machine",
)];

pub const BOOT_SETTING_KINDS: &[Kind] = &[
    Kind {
        id: "boot-loader",
        doc: "the boot loader itself",
        backends: &["android-bootloader"],
        modes: &[],
        params: &[],
    },
    Kind {
        id: "slot-flags",
        doc: "the A/B slot flags: active, priority, tries, successful, unbootable",
        backends: &["android-bootloader"],
        modes: &[],
        params: &[],
    },
    Kind {
        id: "firmware-boot-entry",
        doc: "a UEFI boot entry",
        backends: &["uefi-pc"],
        modes: &[],
        params: &[],
    },
    Kind {
        id: "firmware-boot-order",
        doc: "the UEFI boot order",
        backends: &["uefi-pc"],
        modes: &[],
        params: &[],
    },
];

pub const ARTIFACT_ROLES: &[Entry] = &[
    e("base", "the GlassineOS base"),
    e("kernel", "the kernel (with its boot image where the device needs one)"),
    e("boot-loader", "a boot loader"),
    e("recovery", "the GlassineOS recovery"),
    e("partition-layout", "the partition layout the installation makes"),
];

pub const STEP_KINDS: &[Kind] = &[
    Kind {
        id: "back-up",
        doc: "reads what the step's backs_up names and the boot settings it names, stores them on the computer and checks them by reading back",
        backends: &["android-bootloader", "uefi-pc"],
        modes: &["fastboot", "android-adb", "emergency-download", "local"],
        params: &[],
    },
    Kind {
        id: "write-boot-loader",
        doc: "writes a boot loader artifact over the boot loader partition",
        backends: &["android-bootloader"],
        modes: &["fastboot", "emergency-download"],
        params: &[
            p("artifact", T::Artifact, true, "the boot loader to write"),
            p("partition", T::Partition, true, "the partition it goes to"),
        ],
    },
    Kind {
        id: "write-partition-table",
        doc: "writes a new partition table from the layout artifact: its backup copy first, then the primary",
        backends: &["android-bootloader"],
        modes: &["fastboot", "emergency-download"],
        params: &[
            p("artifact", T::Artifact, true, "the layout"),
            p("renames", T::Renames, false, "partitions the layout renames in place, old = new"),
        ],
    },
    Kind {
        id: "write-image",
        doc: "writes an artifact as a raw image to one partition",
        backends: &["android-bootloader", "uefi-pc"],
        modes: &["fastboot", "emergency-download", "local"],
        params: &[p("artifact", T::Artifact, true, "what is written"), p("partition", T::Partition, true, "where")],
    },
    Kind {
        id: "write-base",
        doc: "puts the base and the first deployment on the system partition",
        backends: &["android-bootloader", "uefi-pc"],
        modes: &["fastboot", "local", "glassine-usb"],
        params: &[
            p("artifact", T::Artifact, true, "the base"),
            p("partition", T::Partition, true, "the system partition"),
        ],
    },
    Kind {
        id: "set-boot-slot",
        doc: "changes the A/B slot flags",
        backends: &["android-bootloader"],
        modes: &["fastboot"],
        params: &[
            p("slot", T::Slot, true, "the slot"),
            p("pin", T::Bool, false, "active, highest priority, marked successful"),
            p("unbootable", T::Bool, false, "marked unbootable, so it is never fallen back to"),
        ],
    },
    Kind {
        id: "create-partitions",
        doc: "creates partitions in the target disk's free space",
        backends: &["uefi-pc"],
        modes: &["local"],
        params: &[p("partitions", T::NewPartitions, true, "the partitions, in order")],
    },
    Kind {
        id: "remove-partitions",
        doc: "removes the partitions of one operating system",
        backends: &["uefi-pc"],
        modes: &["local"],
        params: &[],
    },
    Kind {
        id: "write-boot-files",
        doc: "puts the kernel and the boot loader into a boot partition",
        backends: &["uefi-pc"],
        modes: &["local"],
        params: &[
            p("artifact", T::Artifact, true, "the kernel"),
            p("partition", T::Partition, true, "the boot partition"),
        ],
    },
    Kind {
        id: "add-boot-entry",
        doc: "adds a firmware boot entry for GlassineOS and puts it first in the boot order",
        backends: &["uefi-pc"],
        modes: &["local"],
        params: &[p("partition", T::Partition, true, "the partition the entry starts from")],
    },
];

pub fn step_kind(id: &str) -> Option<&'static Kind> {
    STEP_KINDS.iter().find(|k| k.id == id)
}

/// A capability id. `safe_boot`: a device lacking it is refused (001 FR-016).
#[derive(Debug)]
pub struct Capability {
    pub id: &'static str,
    pub doc: &'static str,
    pub safe_boot: bool,
}

pub const CAPABILITIES: &[Capability] = &[
    Capability {
        id: "start-kernel",
        doc: "starts a GlassineOS kernel of the installation's choice",
        safe_boot: true,
    },
    Capability {
        id: "fallback-boot",
        doc: "falls back to the previous boot when a new one isn't accepted",
        safe_boot: false,
    },
    Capability {
        id: "trial-boot",
        doc: "a one-shot trial boot of a new kernel or deployment",
        safe_boot: false,
    },
    Capability {
        id: "recovery",
        doc: "the GlassineOS recovery",
        safe_boot: false,
    },
    Capability {
        id: "keep-stock",
        doc: "keeps the stock system next to GlassineOS",
        safe_boot: false,
    },
    Capability {
        id: "secondary-recovery",
        doc: "the kept system's own recovery stays reachable",
        safe_boot: false,
    },
    Capability {
        id: "choose-at-start",
        doc: "the person picks the system at start-up",
        safe_boot: false,
    },
];

pub const CAPABILITY_STATUS: &[Entry] = &[
    e("have", "the installed device has it"),
    e("lack", "the installed device does not have it"),
    e("needs", "it has it when something else is there (say what in `with`)"),
];

pub const ESCAPE_LEAVES: &[Entry] = &[
    e("previous-system-starts", "the system the device had before starts"),
    e("glassine-starts", "GlassineOS starts"),
    e("installer-can-finish", "the installer can finish the job"),
    e("installer-can-undo", "the installer can undo the job"),
    e("special-hardware", "only special hardware helps (name it in `hardware`)"),
];

pub const JOBS: &[Entry] = &[
    e("install", "install GlassineOS"),
    e("repair", "put boot and recovery back, keep deployments and the person's storage"),
    e("reinstall", "a fresh deployment, keep the person's storage"),
    e("back-to-stock", "restore the backup"),
];

pub const CHOICE_SOURCES: &[Entry] = &[e("fixed", "options listed in the description"), e("disks", "one of the disks inspecting found")];

pub const CHOICE_TEMPLATES: &[Entry] = &[e("systems", "repeated for each operating system inspecting found")];

pub const SYSTEM_KINDS: &[&str] = &["windows", "linux", "macos", "android", "other"];

/// The standard state ids (FR-009); a description may add others.
pub const STANDARD_STATES: &[&str] = &["stock", "prepared", "installed"];

/// The fastboot variables a report shows with their value; any other variable
/// is shown by name only. `<name>` stands for one partition or slot.
pub const REPORT_FASTBOOT_VARS: &[&str] = &[
    "product",
    "variant",
    "hw-revision",
    "unlocked",
    "secure",
    "kernel",
    "max-download-size",
    "slot-count",
    "current-slot",
    "version-bootloader",
    "version-baseband",
    "erase-block-size",
    "logical-block-size",
    "is-userspace",
    "partition-size:<name>",
    "partition-type:<name>",
    "has-slot:<name>",
    "slot-successful:<name>",
    "slot-unbootable:<name>",
    "slot-retry-count:<name>",
];

/// The Android properties a report shows with their value.
pub const REPORT_ANDROID_PROPS: &[&str] = &[
    "ro.product.device",
    "ro.product.model",
    "ro.product.manufacturer",
    "ro.product.brand",
    "ro.board.platform",
    "ro.hardware",
    "ro.build.version.release",
    "ro.build.version.sdk",
    "ro.product.cpu.abi",
    "ro.boot.slot_suffix",
    "ro.treble.enabled",
];

/// Matches `name` against a list where `<name>` (after a `:`) stands for one segment.
pub fn in_name_list(list: &[&str], name: &str) -> bool {
    list.iter().any(|l| match l.strip_suffix("<name>") {
        Some(prefix) => name.len() > prefix.len() && name.starts_with(prefix) && !name[prefix.len()..].contains(':'),
        None => *l == name,
    })
}
