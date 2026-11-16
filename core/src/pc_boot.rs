// SPDX-License-Identifier: GPL-2.0-or-later
//! What the installer puts on a PC's boot partition, as GlassineOS/boot's
//! `docs/boot-partition.md` (format 1) gives the paths and the bytes, and
//! boot 010 says what and why (FR-001, FR-005, FR-034, FR-037):
//!
//! - the loader at its own path and at the removable-media path;
//! - the first kernel set in `glassine/sets/<id>/`: `vmlinuz`, `initrd`
//!   (assembled from the release's parts, below) and `set.rec`;
//! - the recovery image in `glassine/recovery/<id>/`, in the form of a
//!   kernel set, when the release has one;
//! - the release's default splash, `glassine/splash/default.gspl`, checked
//!   as the loader checks it, when the release has one;
//! - the loader record twice, `glassine/loader.a` and `glassine/loader.b`:
//!   the same record with `seq=1`, each slot 16384 bytes.
//!
//! The release's parts of a kernel set's `initrd`, by manifest role: the
//! CPU microcode archives (`microcode`, `microcode-<name>`, ordered by
//! their file names), the GlassineOS initramfs (the step's `initramfs`), the
//! board's configuration (`board`: the `board.conf` the installer packs as
//! `etc/glassine-boot/board.conf`) and the kernel's boot modules
//! (`boot-modules`). A release that hands the `initrd` over assembled gives
//! the step's `initramfs` the kind [`KIND_ASSEMBLED`] and no microcode or
//! boot modules. `board` is named either way: the base command line is its
//! `CMDLINE` and nothing else ([`board_cmdline`]).

use crate::fat::FileSpec;
use crate::release::Release;
use std::collections::{BTreeMap, BTreeSet};

/// The loader's own path on the boot partition (`\EFI\glassine\loader.efi`).
pub const LOADER: &str = "EFI/glassine/loader.efi";
/// The label of the firmware boot entry the installer makes.
pub const ENTRY_LABEL: &str = "GlassineOS";
/// The loader record's two slots, a then b.
pub const LOADER_SLOTS: [&str; 2] = ["glassine/loader.a", "glassine/loader.b"];
/// The size of each slot file.
pub const SLOT_BYTES: usize = 16384;
/// Where kernel sets go, each in a directory named by its id.
pub const SETS: &str = "glassine/sets";
/// Where recovery images go, the same way.
pub const RECOVERY_DIR: &str = "glassine/recovery";
/// The release's splash.
pub const DEFAULT_SPLASH: &str = "glassine/splash/default.gspl";
/// The boot partition's volume label.
pub const VOLUME_LABEL: &str = "GLASSINE";
/// The longest command line a set is written with (room for the loader's
/// one-shot deployment request).
pub const CMDLINE_MAX: usize = 1920;
/// The command-line keys the installer adds, and the one the loader may.
const OWN_KEYS: [&str; 4] = ["glassine.system=", "glassine.userdata=", "glassine.img=", "glassine.deploy.once="];

/// Manifest roles the boot partition takes when the release has them.
pub const ROLE_MICROCODE: &str = "microcode";
pub const ROLE_BOARD: &str = "board";
pub const ROLE_BOOT_MODULES: &str = "boot-modules";
pub const ROLE_RECOVERY: &str = "recovery";
pub const ROLE_RECOVERY_INITRD: &str = "recovery-initrd";
pub const ROLE_SPLASH: &str = "splash";
/// The kind of an `initramfs` that is a kernel set's whole `initrd`.
pub const KIND_ASSEMBLED: &str = "kernel-set-initrd";
/// The largest `board.conf` read.
pub const BOARD_MAX: u64 = 1 << 20;
/// The largest splash file (32 MiB of picture and its header).
pub const SPLASH_MAX: u64 = (32 << 20) + 64;

/// The removable-media path a firmware falls back to (UEFI 2.x, 3.5.1.1),
/// by the machine's architecture as descriptions name it.
pub fn removable_path(arch: &str) -> Option<&'static str> {
    match arch {
        "x86-64" => Some("EFI/BOOT/BOOTX64.EFI"),
        "arm64" => Some("EFI/BOOT/BOOTAA64.EFI"),
        _ => None,
    }
}

/// A path on the boot partition as the firmware writes it.
pub fn firmware_path(p: &str) -> String {
    format!("\\{}", p.replace('/', "\\"))
}

fn sha_hex(b: &[u8]) -> String {
    crate::ops::hex_sha(b)
}

// ---- records ------------------------------------------------------------------------

/// A record's text with its check line added: the sha256 of every byte
/// before it.
pub fn checked(body: &str) -> String {
    format!("{body}check={}\n", sha_hex(body.as_bytes()))
}

/// A record's keys, if it is valid: text of `key=value` lines each ended by
/// a newline, `format=1` first, no key twice, the check line last and right.
pub fn parse_record(b: &[u8]) -> Result<BTreeMap<String, String>, String> {
    let text = std::str::from_utf8(b).map_err(|_| "not UTF-8".to_string())?;
    if !text.ends_with('\n') || text.contains('\r') {
        return Err("not lines ended by a newline".into());
    }
    let body_end = text[..text.len() - 1].rfind('\n').map(|i| i + 1).unwrap_or(0);
    let (body, last) = text.split_at(body_end);
    let check = last.strip_suffix('\n').and_then(|l| l.strip_prefix("check=")).ok_or("no check line last")?;
    if check != sha_hex(body.as_bytes()) {
        return Err("its check line doesn't match".into());
    }
    let mut keys = BTreeMap::new();
    for (i, line) in body.lines().enumerate() {
        let (k, v) = line.split_once('=').ok_or_else(|| format!("line {} isn't key=value", i + 1))?;
        if k.is_empty() || keys.insert(k.to_string(), v.to_string()).is_some() {
            return Err(format!("line {}: key {k:?} empty or given twice", i + 1));
        }
        if i == 0 && line != "format=1" {
            return Err("its first line isn't format=1".into());
        }
    }
    if keys.is_empty() {
        return Err("no format line".into());
    }
    Ok(keys)
}

/// A slot file: the record, then newlines up to [`SLOT_BYTES`].
pub fn slot_bytes(record: &str) -> Result<Vec<u8>, String> {
    if record.len() > SLOT_BYTES {
        return Err(format!("the loader record is {} bytes, a slot holds {SLOT_BYTES}: not written", record.len()));
    }
    let mut b = record.as_bytes().to_vec();
    b.resize(SLOT_BYTES, b'\n');
    Ok(b)
}

/// A slot's `seq` and keys, if the slot is valid: a valid record, nothing
/// but newlines after its check line, and a decimal `seq`.
pub fn read_slot(b: &[u8]) -> Option<(u64, BTreeMap<String, String>)> {
    let end = b.iter().rposition(|c| *c != b'\n')? + 1;
    if end == b.len() {
        return None; // the check line's newline is missing
    }
    let keys = parse_record(&b[..end + 1]).ok()?;
    let seq = keys.get("seq")?;
    if seq.is_empty() || !seq.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((seq.parse().ok()?, keys))
}

/// The record in use: the valid slot with the higher `seq`, slot a on a tie
/// (0 is a, 1 is b), and whether both slots were valid.
pub fn record_in_use(a: &[u8], b: &[u8]) -> Option<(usize, BTreeMap<String, String>, bool)> {
    match (read_slot(a), read_slot(b)) {
        (Some((sa, ka)), Some((sb, kb))) => Some(if sb > sa { (1, kb, true) } else { (0, ka, true) }),
        (Some((_, k)), None) => Some((0, k, false)),
        (None, Some((_, k))) => Some((1, k, false)),
        (None, None) => None,
    }
}

// ---- kernel sets --------------------------------------------------------------------

/// A kernel set (or the recovery image, in that form): its files' sums and
/// sizes, its command line, its id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelSet {
    /// 16 hex digits (boot-partition.md, "The id").
    pub id: String,
    pub release: String,
    pub kernel_sha256: String,
    pub kernel_size: u64,
    /// The `initrd`'s sum and size; none for a recovery image with its
    /// initramfs built in.
    pub initrd: Option<(String, u64)>,
    /// The whole command line: the base, the identities of boot 010 FR-005
    /// (`glassine.system=`, `glassine.userdata=`) and `glassine.img=`.
    pub cmdline: String,
    /// The identity of the GlassineOS initramfs in the `initrd`; informative.
    pub initramfs: String,
}

/// The id of a set: the first 16 hex digits of the sha256 of three lines,
/// the kernel's sum, the `initrd`'s (empty without one) and the base
/// command line.
pub fn set_id(kernel_sha256: &str, initrd_sha256: Option<&str>, base: &str) -> String {
    let lines = format!("{kernel_sha256}\n{}\n{base}\n", initrd_sha256.unwrap_or(""));
    sha_hex(lines.as_bytes())[..16].to_string()
}

/// A base command line the installer builds on: one line of printable
/// characters, words separated by single spaces, none of the keys it adds.
fn check_base(base: &str) -> Result<(), String> {
    if base.chars().any(|c| c.is_control()) {
        return Err("the release's command line holds a control character (a newline?): not written".into());
    }
    if base.starts_with(' ') || base.ends_with(' ') || base.contains("  ") {
        return Err(format!(
            "the release's command line {base:?} isn't words separated by single spaces: not written"
        ));
    }
    if let Some(k) = base.split(' ').find(|w| OWN_KEYS.iter().any(|k| w.starts_with(k))) {
        return Err(format!("the release's command line already holds {k}: the installer adds it, not written"));
    }
    Ok(())
}

impl KernelSet {
    /// The set of `kernel` (sum, size) and `initrd`, started with `base`
    /// and this installation's partitions.
    pub fn new(
        kernel: (&str, u64),
        initrd: Option<(&str, u64)>,
        base: &str,
        system_partuuid: &str,
        userdata_partuuid: &str,
        release: &str,
        initramfs: &str,
    ) -> Result<KernelSet, String> {
        check_base(base)?;
        for v in [release, initramfs] {
            if v.chars().any(|c| c.is_control()) {
                return Err(format!("{v:?} holds a control character: not written"));
            }
        }
        let id = set_id(kernel.0, initrd.map(|i| i.0), base);
        let mut words: Vec<String> = if base.is_empty() { vec![] } else { vec![base.to_string()] };
        words.push(format!("glassine.system=PARTUUID={}", system_partuuid.to_ascii_lowercase()));
        words.push(format!("glassine.userdata=PARTUUID={}", userdata_partuuid.to_ascii_lowercase()));
        words.push(format!("glassine.img={id}"));
        let cmdline = words.join(" ");
        if cmdline.len() > CMDLINE_MAX {
            return Err(format!(
                "the kernel set's command line is {} bytes, at most {CMDLINE_MAX} are allowed: not written",
                cmdline.len()
            ));
        }
        Ok(KernelSet {
            id,
            release: release.into(),
            kernel_sha256: kernel.0.into(),
            kernel_size: kernel.1,
            initrd: initrd.map(|(s, n)| (s.to_string(), n)),
            cmdline,
            initramfs: initramfs.into(),
        })
    }

    /// The set's record, `set.rec`.
    pub fn record(&self) -> String {
        let (isha, isize) = match &self.initrd {
            Some((s, n)) => (s.clone(), n.to_string()),
            None => (String::new(), String::new()),
        };
        checked(&format!(
            "format=1\nid={}\nrelease={}\nkernel_sha256={}\nkernel_size={}\ninitrd_sha256={isha}\ninitrd_size={isize}\ncmdline={}\ninitramfs={}\n",
            self.id, self.release, self.kernel_sha256, self.kernel_size, self.cmdline, self.initramfs
        ))
    }

    /// The sha256 of `set.rec`, as the loader record names it.
    pub fn record_sha256(&self) -> String {
        sha_hex(self.record().as_bytes())
    }
}

/// The loader record an installation starts with: the set as the default,
/// no previous set, no trial, the recovery image (if any), no request, no
/// unmarked start, no deployment list; `seq=1`.
pub fn loader_record(default: &KernelSet, recovery: Option<&KernelSet>) -> Result<String, String> {
    let (rid, rrec) = recovery.map(|r| (r.id.clone(), r.record_sha256())).unwrap_or_default();
    let r = checked(&format!(
        "format=1\nseq=1\ndefault={}\ndefault_rec={}\nprevious=\nprevious_rec=\ntrial=\ntrial_rec=\nrecovery={rid}\nrecovery_rec={rrec}\nrequest=\nunmarked=0\nstarted=\nstarted_why=\ndeploys=0\n",
        default.id,
        default.record_sha256()
    ));
    slot_bytes(&r)?;
    Ok(r)
}

// ---- the initrd's parts and the release's roles -------------------------------------

/// One part of an assembled `initrd`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    /// An artifact of the release, by role, as it is.
    Role(String),
    /// Bytes the installer made (the board archive, the zeros between parts).
    Bytes(Vec<u8>),
}

/// The roles of the release the boot partition is made from.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BootRoles {
    pub kernel: String,
    pub initramfs: String,
    pub loader: String,
    /// The `initramfs` is the whole `initrd`.
    pub assembled: bool,
    /// In the order they go into the `initrd`.
    pub microcode: Vec<String>,
    pub board: Option<String>,
    pub boot_modules: Option<String>,
    pub recovery: Option<String>,
    pub recovery_initrd: Option<String>,
    pub splash: Option<String>,
}

impl BootRoles {
    /// Every role used, the step's three first.
    pub fn all(&self) -> Vec<String> {
        let mut out = vec![self.kernel.clone(), self.initramfs.clone(), self.loader.clone()];
        out.extend(self.microcode.iter().cloned());
        out.extend(
            [&self.board, &self.boot_modules, &self.recovery, &self.recovery_initrd, &self.splash]
                .into_iter()
                .flatten()
                .cloned(),
        );
        let mut seen = BTreeSet::new();
        out.retain(|r| seen.insert(r.clone()));
        out
    }
}

/// The roles a `write-boot-files` step of `kernel`, `initramfs` and
/// `loader` uses from `release`, or why that release can't make the boot
/// partition.
pub fn boot_roles(release: &Release, kernel: &str, initramfs: &str, loader: &str) -> Result<BootRoles, String> {
    let has = |r: &str| release.artifacts.get(r).is_some_and(|x| x.file.is_some());
    let assembled = release.artifacts.get(initramfs).and_then(|r| r.kind.as_deref()) == Some(KIND_ASSEMBLED);
    let mut micro: Vec<(String, String)> = vec![];
    for (role, r) in &release.artifacts {
        if role == ROLE_MICROCODE || role.starts_with("microcode-") {
            let file = r.file.clone().ok_or_else(|| format!("the release's {role} names no file"))?;
            let name = file.rsplit('/').next().unwrap_or(&file).to_string();
            if micro.iter().any(|(n, _)| *n == name) {
                return Err(format!("two microcode archives are named {name}: their order is not defined"));
            }
            micro.push((name, role.clone()));
        }
    }
    micro.sort();
    let opt = |r: &str| has(r).then(|| r.to_string());
    let out = BootRoles {
        kernel: kernel.into(),
        initramfs: initramfs.into(),
        loader: loader.into(),
        assembled,
        microcode: micro.into_iter().map(|(_, r)| r).collect(),
        board: opt(ROLE_BOARD),
        boot_modules: opt(ROLE_BOOT_MODULES),
        recovery: opt(ROLE_RECOVERY),
        recovery_initrd: opt(ROLE_RECOVERY_INITRD),
        splash: opt(ROLE_SPLASH),
    };
    if out.recovery_initrd.is_some() && out.recovery.is_none() {
        return Err(format!("the release has a {ROLE_RECOVERY_INITRD} but no {ROLE_RECOVERY}"));
    }
    if assembled && (!out.microcode.is_empty() || out.boot_modules.is_some()) {
        return Err(format!(
            "the release's {initramfs} is an assembled initrd ({KIND_ASSEMBLED}) and the release also names microcode or boot modules: which is meant isn't clear"
        ));
    }
    if out.board.is_none() {
        return Err(format!(
            "the release has no {ROLE_BOARD} (the board.conf whose CMDLINE a PC's kernel set starts with): the kernel set can't be made"
        ));
    }
    Ok(out)
}

/// The base command line of a kernel set: `CMDLINE` of `board.conf`, one
/// line of the form `CMDLINE="<text>"` or `CMDLINE=<word>`, with no
/// backslash, `$`, backtick or further quote in it. Any other form, a
/// second `CMDLINE` or none is refused. The text's spaces are checked
/// where the set is made.
pub fn board_cmdline(conf: &[u8]) -> Result<String, String> {
    let text = std::str::from_utf8(conf).map_err(|_| "board.conf isn't UTF-8".to_string())?;
    let mut found: Option<&str> = None;
    for line in text.split('\n') {
        let t = line.trim_start().trim_start_matches("export ").trim_start();
        if !(t.starts_with("CMDLINE=") || t.starts_with("CMDLINE ")) {
            continue;
        }
        if found.is_some() {
            return Err("board.conf sets CMDLINE twice".into());
        }
        let Some(v) = line.strip_prefix("CMDLINE=") else {
            return Err(format!("board.conf's {line:?} isn't CMDLINE=\"<text>\" or CMDLINE=<word>"));
        };
        found = Some(v);
    }
    let v = found.ok_or("board.conf sets no CMDLINE")?;
    let bad = |c: char| matches!(c, '\\' | '$' | '`' | '"' | '\'');
    let value = match v.strip_prefix('"') {
        Some(rest) => rest.strip_suffix('"').filter(|t| !t.contains(bad)),
        None => (!v.is_empty() && !v.contains(bad) && !v.contains(char::is_whitespace)).then_some(v),
    };
    value
        .map(String::from)
        .ok_or_else(|| format!("board.conf's CMDLINE={v} isn't CMDLINE=\"<text>\" or CMDLINE=<word> (no backslash, $, backtick or further quote)"))
}

/// The board archive: an uncompressed newc archive of `etc`,
/// `etc/glassine-boot` and `etc/glassine-boot/board.conf`, owner 0:0, mtime
/// 0, the same bytes `glassine-bootimg-pack` makes.
pub fn board_archive(conf: &[u8]) -> Vec<u8> {
    let entries: [(&str, u32, &[u8]); 4] = [
        ("etc", 0o40755, b""),
        ("etc/glassine-boot", 0o40755, b""),
        ("etc/glassine-boot/board.conf", 0o100644, conf),
        ("TRAILER!!!", 0, b""),
    ];
    let mut out = vec![];
    for (ino, (name, mode, data)) in entries.iter().enumerate() {
        let nb = name.len() + 1;
        let fields = [ino as u32 + 1, *mode, 0, 0, 1, 0, data.len() as u32, 0, 0, 0, 0, nb as u32, 0];
        out.extend_from_slice(b"070701");
        for f in fields {
            out.extend_from_slice(format!("{f:08X}").as_bytes());
        }
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        out.resize(out.len().div_ceil(4) * 4, 0);
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(4) * 4, 0);
    }
    out
}

/// The parts of an assembled `initrd`, with the zeros that put each at an
/// offset that is a multiple of 4: `sizes` gives each role's size.
pub fn initrd_parts(roles: &BootRoles, board: &[u8], size_of: &dyn Fn(&str) -> u64) -> Vec<Part> {
    let mut seq: Vec<Part> = roles.microcode.iter().map(|r| Part::Role(r.clone())).collect();
    seq.push(Part::Role(roles.initramfs.clone()));
    seq.push(Part::Bytes(board_archive(board)));
    if let Some(m) = &roles.boot_modules {
        seq.push(Part::Role(m.clone()));
    }
    let mut out = vec![];
    let mut at = 0u64;
    for p in seq {
        let pad = at.next_multiple_of(4) - at;
        if pad > 0 {
            out.push(Part::Bytes(vec![0; pad as usize]));
            at += pad;
        }
        at += match &p {
            Part::Role(r) => size_of(r),
            Part::Bytes(b) => b.len() as u64,
        };
        out.push(p);
    }
    out
}

// ---- the splash ---------------------------------------------------------------------

/// Checks a default splash as the loader does (boot-partition.md, "The
/// image format"), in its order; a splash the loader would skip, or one of
/// kind 1 (valid only as the override), is refused.
pub fn check_splash(b: &[u8]) -> Result<(), String> {
    let le32 = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let le16 = |o: usize| u16::from_le_bytes(b[o..o + 2].try_into().unwrap());
    if b.len() < 64 {
        return Err(format!("the splash is {} bytes, shorter than its 64-byte header", b.len()));
    }
    if &b[0..8] != b"GLSPLASH" {
        return Err("the splash doesn't start with GLSPLASH".into());
    }
    if crate::table::crc32(&b[0..52]) != le32(52) {
        return Err("the splash's header CRC doesn't match".into());
    }
    if le16(8) != 1 {
        return Err(format!("the splash is format {}, not 1", le16(8)));
    }
    match le16(10) {
        0 => {}
        1 => return Err("the splash is of kind 1 (leave the screen), valid only as the override".into()),
        k => return Err(format!("the splash is of kind {k}, not a picture")),
    }
    let payload = u64::from_le_bytes(b[40..48].try_into().unwrap());
    if payload != b.len() as u64 - 64 {
        return Err(format!(
            "the splash's header gives a payload of {payload} bytes, the file holds {}",
            b.len() - 64
        ));
    }
    let (w, h) = (le32(12) as u64, le32(16) as u64);
    if !(1..=16384).contains(&w) || !(1..=16384).contains(&h) || w * h * 4 > 32 << 20 || b.len() as u64 > SPLASH_MAX {
        return Err(format!("the splash ({w} × {h}, {} bytes) is past the format's limits", b.len()));
    }
    if le32(28) != 1 {
        return Err(format!("the splash's pixel layout is {}, not 1", le32(28)));
    }
    let p = &b[64..];
    if crate::table::crc32(p) != le32(48) {
        return Err("the splash's payload CRC doesn't match".into());
    }
    let pixels = w * h;
    let decoded = match le32(36) {
        0 => (p.len() as u64).is_multiple_of(4).then_some(p.len() as u64 / 4),
        1 => {
            let (mut i, mut n) = (0usize, 0u64);
            let mut ok = true;
            while i < p.len() {
                let c = p[i] as usize;
                let (len, count) = if c < 128 { (1 + (c + 1) * 4, c as u64 + 1) } else { (1 + 4, c as u64 - 126) };
                if i + len > p.len() {
                    ok = false;
                    break;
                }
                i += len;
                n += count;
            }
            ok.then_some(n)
        }
        e => return Err(format!("the splash's encoding is {e}, neither raw nor run-length")),
    };
    if decoded != Some(pixels) {
        return Err(format!("the splash's payload doesn't decode to exactly {w} × {h} pixels"));
    }
    Ok(())
}

// ---- the files ----------------------------------------------------------------------

/// Where a file's bytes come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// An artifact of the release, by role.
    Artifact(String),
    Bytes(Vec<u8>),
    /// An assembled `initrd`.
    Parts(Vec<Part>),
}

/// A set's files under `dir` (`glassine/sets/<id>` or
/// `glassine/recovery/<id>`).
fn set_files(dir: &str, set: &KernelSet, kernel_role: &str, initrd: Option<Content>, out: &mut Vec<(FileSpec, Content)>) {
    let spec = |p: &str, size: u64| FileSpec {
        path: format!("{dir}/{}/{p}", set.id),
        size,
    };
    out.push((spec("vmlinuz", set.kernel_size), Content::Artifact(kernel_role.into())));
    if let (Some((_, size)), Some(c)) = (&set.initrd, initrd) {
        out.push((spec("initrd", *size), c));
    }
    let rec = set.record().into_bytes();
    out.push((spec("set.rec", rec.len() as u64), Content::Bytes(rec)));
}

/// What the boot partition holds and where each file's bytes come from.
#[derive(Debug, Clone)]
pub struct BootFiles {
    pub set: KernelSet,
    pub recovery: Option<KernelSet>,
    pub loader_record: String,
    /// In layout order; the last two are the loader record's slots a and b.
    pub files: Vec<(FileSpec, Content)>,
}

/// The recovery image: its set, its kernel's role, its `initrd`'s role.
pub type Recovery = (KernelSet, String, Option<String>);

/// The boot partition's files: the loader at its two paths, the kernel set
/// (`initrd` its `initrd`'s content), the recovery image, the splash (role,
/// size), and the loader record's two slots last.
pub fn files(
    arch: &str,
    loader: (&str, u64),
    set: &KernelSet,
    kernel_role: &str,
    initrd: Content,
    recovery: Option<&Recovery>,
    splash: Option<(&str, u64)>,
) -> Result<BootFiles, String> {
    let fallback = removable_path(arch).ok_or_else(|| format!("no removable-media path is known for {arch}"))?;
    let spec = |p: &str, size: u64| FileSpec { path: p.into(), size };
    let mut out = vec![
        (spec(LOADER, loader.1), Content::Artifact(loader.0.into())),
        (spec(fallback, loader.1), Content::Artifact(loader.0.into())),
    ];
    if set.initrd.is_none() {
        return Err("a kernel set has an initrd".into());
    }
    set_files(SETS, set, kernel_role, Some(initrd), &mut out);
    if let Some((r, k, i)) = recovery {
        if r.initrd.is_some() != i.is_some() {
            return Err("the recovery image's record and its initrd disagree".into());
        }
        set_files(RECOVERY_DIR, r, k, i.clone().map(Content::Artifact), &mut out);
    }
    if let Some((role, size)) = splash {
        out.push((spec(DEFAULT_SPLASH, size), Content::Artifact(role.into())));
    }
    let lr = loader_record(set, recovery.map(|r| &r.0))?;
    let slot = slot_bytes(&lr)?;
    for p in LOADER_SLOTS {
        out.push((spec(p, SLOT_BYTES as u64), Content::Bytes(slot.clone())));
    }
    Ok(BootFiles {
        set: set.clone(),
        recovery: recovery.map(|r| r.0.clone()),
        loader_record: lr,
        files: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set() -> KernelSet {
        KernelSet::new(("aa", 10), Some(("bb", 20)), "quiet", "1-2", "3-4", "6.18.63-0-lts", "").unwrap()
    }

    #[test]
    fn the_command_line_carries_the_identities_and_the_id() {
        let s = set();
        assert_eq!(
            s.cmdline,
            format!("quiet glassine.system=PARTUUID=1-2 glassine.userdata=PARTUUID=3-4 glassine.img={}", s.id)
        );
        assert_eq!(s.id, sha_hex(b"aa\nbb\nquiet\n")[..16]);
        // the id doesn't depend on the installation's identities
        let t = KernelSet::new(("aa", 10), Some(("bb", 20)), "quiet", "5-6", "7-8", "", "").unwrap();
        assert_eq!(s.id, t.id);
        // no initrd: an empty line
        let r = KernelSet::new(("aa", 10), None, "", "1-2", "3-4", "", "").unwrap();
        assert_eq!(r.id, sha_hex(b"aa\n\n\n")[..16]);
        assert!(r.cmdline.starts_with("glassine.system="));
        let f = files("x86-64", ("boot-loader", 5), &s, "kernel", Content::Artifact("initramfs".into()), None, None).unwrap();
        assert_eq!(f.files[1].0.path, "EFI/BOOT/BOOTX64.EFI");
        assert_eq!(f.files[3].0.path, format!("glassine/sets/{}/initrd", s.id));
        assert!(files("riscv64", ("boot-loader", 5), &s, "kernel", Content::Artifact("x".into()), None, None).is_err());
    }

    #[test]
    fn a_command_line_that_breaks_the_rules_is_refused() {
        let mk = |base: &str| KernelSet::new(("aa", 1), Some(("bb", 1)), base, "1", "2", "", "");
        assert!(mk("a\nb").is_err());
        assert!(mk(" quiet").is_err() && mk("quiet ").is_err() && mk("a  b").is_err());
        assert!(mk("quiet glassine.img=x").is_err());
        assert!(mk("glassine.system=PARTUUID=x").is_err());
        let uuid = "00000000-0000-0000-0000-000000000000";
        let tail = format!(" glassine.system=PARTUUID={uuid} glassine.userdata=PARTUUID={uuid} glassine.img=0123456789abcdef").len();
        let fits = "x".repeat(CMDLINE_MAX - tail);
        let one = KernelSet::new(("aa", 1), Some(("bb", 1)), &fits, uuid, uuid, "", "").unwrap();
        assert_eq!(one.cmdline.len(), CMDLINE_MAX);
        assert!(KernelSet::new(("aa", 1), Some(("bb", 1)), &format!("{fits}x"), uuid, uuid, "", "").is_err());
    }

    #[test]
    fn a_slot_is_valid_only_whole() {
        let r = loader_record(&set(), None).unwrap();
        let slot = slot_bytes(&r).unwrap();
        assert_eq!(slot.len(), SLOT_BYTES);
        let (seq, keys) = read_slot(&slot).unwrap();
        assert_eq!(seq, 1);
        assert_eq!(keys["default"], set().id);
        // a cut at any byte of slot b's write, over zeros or over other
        // bytes: slot b isn't valid and slot a is the record in use
        for fill in [0u8, 0xa5] {
            for k in 0..SLOT_BYTES {
                let mut b = vec![fill; SLOT_BYTES];
                b[..k].copy_from_slice(&slot[..k]);
                assert!(read_slot(&b).is_none(), "cut at {k} over {fill:#x}");
                let (which, keys, both) = record_in_use(&slot, &b).unwrap();
                assert!(which == 0 && !both && keys["seq"] == "1");
            }
        }
        // the higher seq wins; a tie goes to a
        let two = slot_bytes(&checked(&r[..r.rfind("check=").unwrap()].replace("seq=1\n", "seq=2\n"))).unwrap();
        assert_eq!(record_in_use(&slot, &two).unwrap().0, 1);
        assert_eq!(record_in_use(&two, &slot).unwrap().0, 0);
        assert_eq!(record_in_use(&slot, &slot).unwrap().0, 0);
        // a record that doesn't fit its slot isn't written
        assert!(slot_bytes(&"x".repeat(SLOT_BYTES + 1)).is_err());
    }

    #[test]
    fn cmdline_is_read_from_board_conf_in_its_two_forms_only() {
        let c = |t: &str| board_cmdline(t.as_bytes());
        assert_eq!(c("ROOT=x\nCMDLINE=\"console=ttyS0 quiet\"\n").unwrap(), "console=ttyS0 quiet");
        assert_eq!(c("CMDLINE=quiet\n").unwrap(), "quiet");
        assert_eq!(c("CMDLINE=\"\"\n").unwrap(), "");
        assert_eq!(c("CMDLINE=\"quiet\"").unwrap(), "quiet");
        for bad in [
            "ROOT=x\n",
            "CMDLINE=a\nCMDLINE=b\n",
            "CMDLINE='quiet'\n",
            "CMDLINE=\"a\\\"b\"\n",
            "CMDLINE=\"$X\"\n",
            "CMDLINE=\"`x`\"\n",
            "CMDLINE=\"a\" # c\n",
            "CMDLINE=a b\n",
            "CMDLINE=\"a\n",
            "export CMDLINE=a\n",
            " CMDLINE=a\n",
            "CMDLINE=\n",
            "CMDLINE=\"a'b\"\n",
        ] {
            assert!(c(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_board_archive_is_glassine_bootimg_packs() {
        let a = board_archive(b"CMDLINE=quiet\n");
        assert_eq!(a.len() % 4, 0);
        assert!(a.starts_with(b"07070100000001000041ED"));
        assert!(a.windows(10).any(|w| w == b"TRAILER!!!"));
    }

    fn splash(w: u32, h: u32, kind: u16, enc: u32, payload: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 64];
        b[0..8].copy_from_slice(b"GLSPLASH");
        b[8..10].copy_from_slice(&1u16.to_le_bytes());
        b[10..12].copy_from_slice(&kind.to_le_bytes());
        b[12..16].copy_from_slice(&w.to_le_bytes());
        b[16..20].copy_from_slice(&h.to_le_bytes());
        b[28..32].copy_from_slice(&1u32.to_le_bytes());
        b[36..40].copy_from_slice(&enc.to_le_bytes());
        b[40..48].copy_from_slice(&(payload.len() as u64).to_le_bytes());
        b[48..52].copy_from_slice(&crate::table::crc32(payload).to_le_bytes());
        let c = crate::table::crc32(&b[0..52]);
        b[52..56].copy_from_slice(&c.to_le_bytes());
        b.extend_from_slice(payload);
        b
    }

    #[test]
    fn a_splash_is_checked_as_the_loader_checks_it() {
        assert!(check_splash(&splash(2, 2, 0, 0, &[7; 16])).is_ok());
        // run-length: 2 as they are, then one repeated twice
        assert!(check_splash(&splash(2, 2, 0, 1, &[1, 1, 1, 1, 1, 2, 2, 2, 2, 128, 3, 3, 3, 3])).is_ok());
        assert!(check_splash(&splash(2, 2, 0, 1, &[129, 3, 3, 3, 3])).is_err(), "3 pixels, not 4");
        assert!(check_splash(&splash(2, 2, 0, 0, &[7; 12])).is_err());
        assert!(check_splash(&splash(2, 2, 1, 0, &[])).is_err(), "kind 1 is no default splash");
        assert!(check_splash(&splash(0, 2, 0, 0, &[])).is_err());
        assert!(check_splash(&splash(2, 2, 0, 2, &[7; 16])).is_err());
        let mut bad = splash(2, 2, 0, 0, &[7; 16]);
        bad[70] ^= 1;
        assert!(check_splash(&bad).unwrap_err().contains("payload CRC"));
        bad = splash(2, 2, 0, 0, &[7; 16]);
        bad[12] = 3;
        assert!(check_splash(&bad).unwrap_err().contains("header CRC"));
        assert!(check_splash(b"GLSPLASH").is_err());
    }
}
