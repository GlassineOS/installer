// SPDX-License-Identifier: GPL-2.0-or-later
//! The files of the installed system that state a device fact the description
//! holds (spec 002 FR-030), derived from the description: the device-data list
//! (FR-018, boot 006 FR-001) and the slot of the system an installation keeps
//! next to GlassineOS. The device's package runs this at its build, so no such
//! file is written by hand.

use crate::format1::{Description, DeviceDataFile, DeviceDatum};

/// Where the installed system holds its description (FR-029).
pub const INSTALLED_AT: &str = "/usr/share/glassine/device.toml";

/// A derived file: its name, where the installed system reads it, its content.
#[derive(Debug, Clone, PartialEq)]
pub struct Derived {
    pub file: &'static str,
    pub installed_at: &'static str,
    pub content: String,
}

/// Every file the description derives; a file it has nothing for is left out
/// (a device with no device data ships no list, boot 006 FR-002).
pub fn derive(d: &Description) -> Result<Vec<Derived>, String> {
    let mut out = Vec::new();
    if let Some(content) = device_data_list(d)? {
        out.push(Derived {
            file: "device-data.list",
            installed_at: "/usr/share/glassine/device-data.list",
            content,
        });
    }
    if let Some(content) = slots_conf(d)? {
        out.push(Derived {
            file: "slots.conf",
            installed_at: "/etc/glassine/slots.conf",
            content,
        });
    }
    Ok(out)
}

const WIDTH: usize = 88;

/// `# ` lines of `text`, wrapped at [`WIDTH`] columns.
fn comment(o: &mut String, text: &str) {
    let mut line = String::from("#");
    for w in text.split_whitespace() {
        if line.len() > 1 && line.len() + 1 + w.len() > WIDTH {
            o.push_str(&line);
            o.push('\n');
            line = String::from("#");
        }
        line.push(' ');
        line.push_str(w);
    }
    o.push_str(&line);
    o.push('\n');
}

/// The device-data list in boot's format 1 (GlassineOS/boot docs/kernel-artifacts.md):
/// one `NAME PART:/PATH [CHECK]` line per file, in the description's order, each
/// group of files with one `what` under that text as a comment. `None` when the
/// description has no device data.
pub fn device_data_list(d: &Description) -> Result<Option<String>, String> {
    let mut files: Vec<&DeviceDataFile> = Vec::new();
    for x in &d.device_data {
        match x {
            DeviceDatum::File(f) => files.push(f),
            DeviceDatum::Named(n) => {
                return Err(format!(
                    "device_data {n:?} is only a name (format {}); a list is derived from format 2's entries",
                    d.format
                ));
            }
        }
    }
    if files.is_empty() {
        return Ok(None);
    }
    let source = |f: &DeviceDataFile| format!("{}:{}", f.partition, f.path);
    let any_check = files.iter().any(|f| f.check.is_some());
    let w_name = files.iter().map(|f| f.name.len()).max().unwrap_or(0).max("# NAME".len()) + 1;
    let w_source = files.iter().map(|f| source(f).len()).max().unwrap_or(0) + 1;

    let mut o = String::from("format=1\n");
    comment(
        &mut o,
        &format!(
            "The {}'s device data: files the seed copies once from the device's own partitions \
             (read-only) to glassine/ on the board's DEVICE_PART. The format is in GlassineOS/boot \
             docs/kernel-artifacts.md.",
            d.name
        ),
    );
    comment(
        &mut o,
        &format!(
            "Derived from the device's description ({INSTALLED_AT}, [[device_data]]) when its \
             package is built: change the description, never this file."
        ),
    );
    let header = if any_check {
        format!("{:<w_name$}{:<w_source$}CHECK", "# NAME", "SOURCE")
    } else {
        format!("{:<w_name$}SOURCE", "# NAME")
    };
    o.push_str(&header);
    o.push('\n');
    let mut group: Option<&str> = None;
    for f in files {
        if group != Some(f.what.as_str()) {
            comment(&mut o, &f.what);
            group = Some(f.what.as_str());
        }
        let line = match &f.check {
            Some(c) => format!("{:<w_name$}{:<w_source$}{c}", f.name, source(f)),
            None => format!("{:<w_name$}{}", f.name, source(f)),
        };
        o.push_str(&line);
        o.push('\n');
    }
    Ok(Some(o))
}

/// The slot the system an installation may keep next to GlassineOS starts from:
/// the slot of the step that, when the person chooses not to keep that system
/// (a choice's option that replaces it), marks it unbootable. `None` when the
/// description keeps no other system on a slot.
pub fn kept_system_slot(d: &Description) -> Result<Option<String>, String> {
    let replaces = |c: &str, o: &str| d.choice(c).is_some_and(|c| c.options.iter().any(|x| x.id == o && x.replaces_system));
    let mut slots: Vec<String> = Vec::new();
    for s in &d.steps {
        if s.kind != "set-boot-slot" || s.params.get("unbootable").and_then(|v| v.as_bool()) != Some(true) {
            continue;
        }
        if !s.when.iter().any(|w| replaces(&w.choice, &w.equals)) {
            continue;
        }
        let Some(slot) = s.params.get("slot").and_then(|v| v.as_str()) else {
            return Err(format!("step {:?} marks a slot unbootable but names none", s.id));
        };
        if !slots.iter().any(|x| x == slot) {
            slots.push(slot.to_string());
        }
    }
    match slots.len() {
        0 => Ok(None),
        1 => Ok(slots.pop()),
        _ => Err(format!("the kept system's slot is ambiguous: the steps retire slots {}", slots.join(", "))),
    }
}

/// `slots.conf` for the shell's power menu (glassine-power-root): `ANDROID_SLOT=`
/// the kept system's slot. `None` when there is none.
pub fn slots_conf(d: &Description) -> Result<Option<String>, String> {
    let Some(slot) = kept_system_slot(d)? else {
        return Ok(None);
    };
    let mut o = String::new();
    comment(
        &mut o,
        &format!(
            "The {}'s second system, the one an installation keeps next to GlassineOS: the slot it \
             starts from, for the shell's power menu entry \"Android\" (glassine-power-root). \
             glassine-slot makes it active and marks it successful, then the device reboots into it.",
            d.name
        ),
    );
    comment(
        &mut o,
        &format!("Derived from the device's description ({INSTALLED_AT}) when its package is built: change the description, never this file."),
    );
    o.push_str(&format!("ANDROID_SLOT={slot}\n"));
    Ok(Some(o))
}
