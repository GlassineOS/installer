// SPDX-License-Identifier: GPL-2.0-or-later
//! `glassine-install restore --pc --backup DIR [--disk N] [--root DIR]`: puts a
//! PC backup (the one `install --pc` made) back onto the disk it was made of,
//! and the firmware's boot entries and boot order it holds. The whole stored
//! copy is checked against the manifest before the first write; each range is
//! written only where the disk differs and read back. A cut restore is run
//! again with the same command and goes on where the disk still differs.

use super::{Opts, USAGE, opts};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

pub fn restore_cmd(args: &[String]) -> Result<ExitCode, String> {
    let o: Opts = opts(args, &["--pc"], &["--backup", "--disk", "--root"])?;
    if !o.has("--pc") {
        return Err(format!("restore puts back a PC's backup: it needs --pc\n\n{USAGE}"));
    }
    let backup = PathBuf::from(o.one("--backup").ok_or(format!("restore needs --backup DIR\n\n{USAGE}"))?);
    let root = PathBuf::from(o.one("--root").map(String::as_str).unwrap_or("/"));
    let disk = match o.one("--disk") {
        Some(d) => Some(
            d.parse::<u32>()
                .map_err(|_| format!("--disk {d}: a disk's number (as inspect --pc lists it)"))?,
        ),
        None => None,
    };
    run(&root, &backup, disk)
}

#[cfg(target_os = "linux")]
fn run(root: &Path, backup: &Path, disk: Option<u32>) -> Result<ExitCode, String> {
    use glassine_installer::pc_execute::Machine;
    use glassine_installer::pc_image;
    use glassine_installer_pcwrite::{DirEdge, LinuxMachine};
    use std::io::BufRead;
    const CONFIRM: &str = "restore";
    let mut edge = DirEdge::new(&backup.join(".restore-record.json"), Some(backup.to_path_buf()), None);
    let (man, sha) = pc_image::manifest(&mut edge)?;
    if man.disks.len() != 1 {
        return Err(format!(
            "the backup holds {} disk images; restore puts back a backup of one disk",
            man.disks.len()
        ));
    }
    let bd = &man.disks[0];
    let target = disk.unwrap_or(bd.disk.index);
    let abs = std::fs::canonicalize(backup).map_err(|e| format!("{}: {e}", backup.display()))?;
    if glassine_installer_pc::disk_of(root, &abs) == Some(target) {
        return Err(format!(
            "the backup ({}) is on disk {target}, the disk it would be written to",
            backup.display()
        ));
    }
    let mut m = LinuxMachine::new(root, target)?;
    let now = m
        .disks()?
        .into_iter()
        .find(|d| d.index == target)
        .ok_or_else(|| format!("disk {target} isn't there"))?;
    println!("backup {} (manifest sha256 {sha}, format {})", backup.display(), man.backup_format);
    println!(
        "  of disk {} ({} bytes, {}{})",
        bd.disk.index,
        bd.disk.size,
        bd.disk.kind,
        bd.disk.model.as_deref().map(|m| format!(", {m}")).unwrap_or_default()
    );
    if let Some(im) = &bd.image {
        println!(
            "  holds {} bytes of data and {} of zeros; {} bytes of free space in its file systems aren't kept",
            im.data_bytes, im.zero_bytes, im.free_bytes
        );
    }
    println!(
        "restore onto disk {target} ({} bytes, {}{}): every partition and the partition table as they were;",
        now.size,
        now.kind,
        now.model.as_deref().map(|m| format!(", {m}")).unwrap_or_default()
    );
    println!("  whatever is on that disk now is replaced. Then the firmware's boot entries and boot order it holds.");
    if (now.kind.as_str(), &now.model) != (bd.disk.kind.as_str(), &bd.disk.model) {
        println!("  note: disk {target} isn't of the same kind and model as the disk backed up");
    }
    println!("type \"{CONFIRM}\" to go on:");
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).map_err(|e| e.to_string())?;
    if line.trim() != CONFIRM {
        println!("not restored: nothing was written");
        return Ok(ExitCode::from(1));
    }
    let r = pc_image::restore_disk(&mut m, &mut edge, bd, man.backup_format, target).and_then(|mut r| {
        pc_image::restore_variables(&mut m, &mut edge, &man, &mut r)?;
        Ok(r)
    });
    match r {
        Ok(r) => {
            println!(
                "restored: {} bytes written, {} already in place; boot settings written: {}",
                r.written_bytes,
                r.same_bytes,
                if r.variables_written.is_empty() {
                    "none".to_string()
                } else {
                    r.variables_written.join(", ")
                }
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            eprintln!("{e}");
            eprintln!("run the same restore again: it goes on where the disk still differs");
            Ok(ExitCode::from(1))
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn run(_: &Path, _: &Path, _: Option<u32>) -> Result<ExitCode, String> {
    Err("restoring a PC's backup works on Linux only".into())
}
