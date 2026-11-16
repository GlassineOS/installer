// SPDX-License-Identifier: GPL-2.0-or-later
//! What the installer puts on a PC's boot partition (boot 010 FR-001,
//! FR-005, FR-034): the loader at its own path and at the removable-media
//! path, the first kernel set (kernel, initramfs, command line) under a
//! name made from its id, that set's kernel record, and the loader record
//! naming it as the default set.
//!
//! PROVISIONAL: boot 010 leaves the loader record's form and the boot
//! partition's fixed names to `docs/kernel-artifacts.md` (its contracts
//! table: "to be added"). Until boot specifies them, the names and the
//! record below are this installer's stand-in, each record marked
//! `provisional=1`; a change on boot's side changes only this module.

use crate::fat::FileSpec;
use sha2::{Digest, Sha256};

/// The loader's own path on the boot partition (`\EFI\glassine\loader.efi`).
pub const LOADER: &str = "EFI/glassine/loader.efi";
/// The label of the firmware boot entry the installer makes.
pub const ENTRY_LABEL: &str = "GlassineOS";
/// The loader record.
pub const LOADER_RECORD: &str = "glassine/loader.rec";
/// The boot partition's volume label.
pub const VOLUME_LABEL: &str = "GLASSINE";

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

/// The first kernel set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelSet {
    /// 8 hex digits, from the kernel, the initramfs and the release's
    /// command line (not the identities: they differ per installation).
    pub id: String,
    pub kernel_sha256: String,
    pub kernel_size: u64,
    pub initramfs_sha256: String,
    pub initramfs_size: u64,
    /// The whole command line: the release's, the identities of boot 001
    /// FR-020 (`glassine.system=`, `glassine.userdata=`) and `glassine.img=`.
    pub cmdline: String,
    pub release: Option<String>,
}

impl KernelSet {
    pub fn new(
        kernel: (&str, u64),
        initramfs: (&str, u64),
        release_cmdline: &str,
        system_partuuid: &str,
        userdata_partuuid: &str,
        release: Option<&str>,
    ) -> KernelSet {
        let mut h = Sha256::new();
        h.update(b"glassine kernel set\0");
        h.update(kernel.0.as_bytes());
        h.update(b"\0");
        h.update(initramfs.0.as_bytes());
        h.update(b"\0");
        h.update(release_cmdline.as_bytes());
        let id = crate::ops::hex(&h.finalize()[..4]);
        let mut cmdline = release_cmdline.trim().to_string();
        for kv in [
            format!("glassine.system=PARTUUID={system_partuuid}"),
            format!("glassine.userdata=PARTUUID={userdata_partuuid}"),
            format!("glassine.img={id}"),
        ] {
            if !cmdline.is_empty() {
                cmdline.push(' ');
            }
            cmdline += &kv;
        }
        KernelSet {
            id,
            kernel_sha256: kernel.0.into(),
            kernel_size: kernel.1,
            initramfs_sha256: initramfs.0.into(),
            initramfs_size: initramfs.1,
            cmdline,
            release: release.map(String::from),
        }
    }

    pub fn kernel_path(&self) -> String {
        format!("glassine/{}/vmlinuz", self.id)
    }
    pub fn initramfs_path(&self) -> String {
        format!("glassine/{}/initrd", self.id)
    }
    pub fn record_path(&self) -> String {
        format!("glassine/{}/set.rec", self.id)
    }

    /// The set's kernel record: the fields of a boot 004 image record.
    pub fn kernel_record(&self) -> String {
        let mut s = String::from("format=1\nprovisional=1\n");
        s += &format!("id={}\n", self.id);
        s += &format!("release={}\n", self.release.as_deref().unwrap_or(""));
        s += &format!(
            "kernel={}\nkernel_sha256={}\nkernel_size={}\n",
            firmware_path(&self.kernel_path()),
            self.kernel_sha256,
            self.kernel_size
        );
        s += &format!(
            "initramfs={}\ninitramfs_sha256={}\ninitramfs_size={}\n",
            firmware_path(&self.initramfs_path()),
            self.initramfs_sha256,
            self.initramfs_size
        );
        s += &format!("cmdline={}\nseeded=1\n", self.cmdline);
        s
    }

    /// The loader record: this set as the default, no previous set, no
    /// request, no unmarked start, and its own check value last (the
    /// sha256 of the lines before it).
    pub fn loader_record(&self) -> String {
        let mut s = String::from("format=1\nprovisional=1\n");
        s += &format!("default={}\n", self.id);
        s += &format!("default_record={}\n", firmware_path(&self.record_path()));
        s += &format!("default_record_sha256={}\n", crate::ops::hex_sha(self.kernel_record().as_bytes()));
        s += &format!("kernel={}\nkernel_sha256={}\n", firmware_path(&self.kernel_path()), self.kernel_sha256);
        s += &format!("initrd={}\ninitrd_sha256={}\n", firmware_path(&self.initramfs_path()), self.initramfs_sha256);
        s += &format!("cmdline={}\n", self.cmdline);
        s += "previous=\nrequest=\nunmarked=0\n";
        let check = crate::ops::hex_sha(s.as_bytes());
        s + &format!("check={check}\n")
    }
}

/// Where a file's bytes come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    /// An artifact of the release, by role.
    Artifact(String),
    Bytes(Vec<u8>),
}

/// The boot partition's files, in the order they are laid out.
pub fn files(
    set: &KernelSet,
    loader_role: &str,
    loader_size: u64,
    kernel_role: &str,
    initramfs_role: &str,
    arch: &str,
) -> Result<Vec<(FileSpec, Content)>, String> {
    let fallback = removable_path(arch).ok_or_else(|| format!("no removable-media path is known for {arch}"))?;
    let spec = |p: &str, size: u64| FileSpec { path: p.into(), size };
    let kr = set.kernel_record().into_bytes();
    let lr = set.loader_record().into_bytes();
    Ok(vec![
        (spec(LOADER, loader_size), Content::Artifact(loader_role.into())),
        (spec(fallback, loader_size), Content::Artifact(loader_role.into())),
        (spec(&set.kernel_path(), set.kernel_size), Content::Artifact(kernel_role.into())),
        (spec(&set.initramfs_path(), set.initramfs_size), Content::Artifact(initramfs_role.into())),
        (spec(&set.record_path(), kr.len() as u64), Content::Bytes(kr)),
        (spec(LOADER_RECORD, lr.len() as u64), Content::Bytes(lr)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_carries_the_identities_and_the_id() {
        let s = KernelSet::new(("aa", 10), ("bb", 20), "quiet", "1-2", "3-4", Some("6.18.63-0-lts"));
        assert_eq!(
            s.cmdline,
            format!("quiet glassine.system=PARTUUID=1-2 glassine.userdata=PARTUUID=3-4 glassine.img={}", s.id)
        );
        assert_eq!(s.id.len(), 8);
        // the id doesn't depend on the installation's identities
        let t = KernelSet::new(("aa", 10), ("bb", 20), "quiet", "5-6", "7-8", None);
        assert_eq!(s.id, t.id);
        let r = s.loader_record();
        let (body, check) = r.trim_end().rsplit_once('\n').unwrap();
        assert_eq!(check, format!("check={}", crate::ops::hex_sha(format!("{body}\n").as_bytes())));
        assert!(r.contains(&format!("kernel=\\glassine\\{}\\vmlinuz\n", s.id)));
        let f = files(&s, "boot-loader", 5, "kernel", "initramfs", "x86-64").unwrap();
        assert_eq!(f[1].0.path, "EFI/BOOT/BOOTX64.EFI");
        assert!(files(&s, "boot-loader", 5, "kernel", "initramfs", "riscv64").is_err());
    }
}
