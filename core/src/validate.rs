// SPDX-License-Identifier: GPL-2.0-or-later
//! Loading and checking descriptions: structure (FR-002, against the shape),
//! then every rule of the format that needs more than the structure (FR-007a,
//! FR-008, FR-009, FR-013, FR-014, FR-014a, FR-016a, FR-018b, FR-021, FR-022,
//! FR-023, FR-023a, FR-025, FR-026 and what a description never holds).

use crate::format1::{self, Condition, Description, Scalar, Step, When};
use crate::shape::{Problem, problem};
use crate::vocab::{self, FactType};
use std::collections::{BTreeMap, BTreeSet};

/// What reading a description gives.
#[derive(Debug)]
pub enum Loaded {
    Valid(Box<Description>),
    /// A format later than this reader's: refused by name, never read partly (Scenario 2).
    TooNew {
        format: i64,
    },
    Invalid(Vec<Problem>),
}

/// The highest format this reader reads; it reads every earlier one too (FR-003).
pub const READS_UP_TO: i64 = format1::FORMAT;

pub fn load(text: &str) -> Loaded {
    let tab: toml::Table = match text.parse() {
        Ok(t) => t,
        Err(e) => return Loaded::Invalid(vec![problem("", format!("not TOML: {}", e.message()))]),
    };
    let format = match tab.get("format") {
        Some(toml::Value::Integer(n)) => *n,
        Some(_) => return Loaded::Invalid(vec![problem("format", "wrong type: expected an integer")]),
        None => return Loaded::Invalid(vec![problem("format", "missing required key")]),
    };
    if format > READS_UP_TO {
        return Loaded::TooNew { format };
    }
    let mut out = Vec::new();
    if format < format1::FIRST_FORMAT {
        out.push(problem("format", format!("unknown format {format}")));
        return Loaded::Invalid(out);
    }
    if tab.keys().next().map(String::as_str) != Some("format") {
        out.push(problem("format", "must be the first key"));
    }
    format1::shape_for(format).walk(&tab, "", &mut out);
    // The rules beyond the structure run whenever the tree still reads as
    // its format (an unknown value or a missing parameter does), so a person
    // sees every problem at once; an unknown key or a wrong type stops them.
    let structural = !out.is_empty();
    let d: Option<Description> = toml::Value::Table(tab).try_into().ok();
    match &d {
        Some(d) => out.extend(check(d)),
        None if structural => {}
        None => out.push(problem("", format!("does not read as format {format}"))),
    }
    let allowed = d.as_ref().map(allowed_hashes).unwrap_or_default();
    out.extend(never_holds_except(text, &allowed));
    match d {
        Some(d) if out.is_empty() => Loaded::Valid(Box::new(d)),
        _ => {
            out.sort();
            out.dedup();
            Loaded::Invalid(out)
        }
    }
}

struct Ck<'a> {
    d: &'a Description,
    out: Vec<Problem>,
}

impl Ck<'_> {
    fn p(&mut self, path: impl AsRef<str>, msg: impl Into<String>) {
        self.out.push(problem(path.as_ref(), msg));
    }
}

fn unique<'a>(ck: &mut Ck, list: impl Iterator<Item = &'a str>, base: &str, what: &str) {
    let mut seen = BTreeSet::new();
    for (i, id) in list.enumerate() {
        if !seen.insert(id) {
            ck.p(format!("{base}[{i}].{what}"), format!("{id:?} appears twice"));
        }
    }
}

/// A text table, and whether each of its keys names something that exists.
type TextMap<'a> = (&'a str, &'a BTreeMap<String, String>, Box<dyn Fn(&str) -> bool + 'a>);

/// Every rule beyond the structure, on one description.
pub fn check(d: &Description) -> Vec<Problem> {
    let mut ck = Ck { d, out: Vec::new() };
    let backend = d.backend.as_str();

    // Who the device is (FR-005..FR-008).
    if d.name.trim().is_empty() {
        ck.p("name", "is empty");
    }
    if d.generic && d.refines.is_some() {
        ck.p("refines", "a generic description refines nothing");
    }
    if d.refines.as_deref() == Some(d.id.as_str()) {
        ck.p("refines", "a description can't refine itself");
    }
    let state_keys: BTreeSet<&str> = d.states.iter().flat_map(|s| s.conditions.iter().map(|c| c.key.as_str())).collect();
    let mut required_kinds = BTreeSet::new();
    let mut required = 0;
    for (i, c) in d.identity.iter().enumerate() {
        let path = format!("identity[{i}]");
        check_test(&mut ck, c, &path);
        if let Some((fk, _)) = vocab::fact_key(&c.key) {
            if !fk.modes.iter().any(|m| d.modes.iter().any(|x| x.kind == *m)) {
                ck.p(format!("{path}.key"), "no mode of this description can observe this fact");
            }
            if c.required == Some(true) {
                required += 1;
                required_kinds.insert(fk.kind);
            }
        }
        if state_keys.contains(c.key.as_str()) {
            ck.p(
                format!("{path}.key"),
                "this fact also tells a state; a signal that changes with the install state belongs to the states (FR-008)",
            );
        }
        check_tried(&mut ck, &c.verified, &path);
    }
    if required == 0 {
        ck.p("identity", "needs at least one required signal");
    } else if !d.generic && (required < 2 || required_kinds.len() < 2) {
        ck.p("identity", "needs at least two required signals of different kinds (FR-008), or generic = true");
    }

    // States (FR-009).
    unique(&mut ck, d.states.iter().map(|s| s.id.as_str()), "states", "id");
    if d.states.first().map(|s| s.id.as_str()) != Some("stock") {
        ck.p("states[0].id", "the first state is the device as its maker ships it: stock");
    }
    for (i, s) in d.states.iter().enumerate() {
        for (j, c) in s.conditions.iter().enumerate() {
            check_moded(&mut ck, c, &format!("states[{i}].conditions[{j}]"));
        }
    }

    // Modes (FR-010..FR-012).
    unique(&mut ck, d.modes.iter().map(|m| m.id.as_str()), "modes", "id");
    for (i, m) in d.modes.iter().enumerate() {
        let path = format!("modes[{i}]");
        if m.id == vocab::INSTALLER_MODE {
            ck.p(format!("{path}.id"), "`installer` is reserved for the installer's own records");
        }
        if let Some(n) = &m.needs {
            if d.supplied_file(n).is_none() {
                ck.p(format!("{path}.needs"), format!("no supplied file {n:?}"));
            }
            if vocab::supplied_matches(&m.kind).is_empty() {
                ck.p(format!("{path}.needs"), format!("a mode of kind {:?} runs no supplied program", m.kind));
            }
        }
        match vocab::mode_kind(&m.kind) {
            Some(k) if !k.backends.contains(&backend) => ck.p(
                format!("{path}.kind"),
                format!("the {backend} backend doesn't implement mode kind {:?}", m.kind),
            ),
            Some(k) => {
                for (j, dep) in m.departures.iter().enumerate() {
                    let dp = format!("{path}.departures[{j}]");
                    if let Some(dk) = vocab::DEPARTURES.iter().find(|x| x.id == dep.kind) {
                        if !dk.backends.contains(&backend) {
                            ck.p(
                                format!("{dp}.kind"),
                                format!("the {backend} backend doesn't implement departure {:?}", dep.kind),
                            );
                        }
                        if !dk.modes.contains(&m.kind.as_str()) {
                            ck.p(
                                format!("{dp}.kind"),
                                format!("departure {:?} doesn't apply to mode kind {:?}", dep.kind, m.kind),
                            );
                        }
                    }
                    if let Some(toml::Value::String(op)) = dep.params.get("operation")
                        && !k.operations.contains(&op.as_str())
                    {
                        ck.p(format!("{dp}.params.operation"), format!("mode kind {:?} has no operation {op:?}", m.kind));
                    }
                    if let Some(t) = &dep.told_by {
                        check_test(&mut ck, t, &format!("{dp}.told_by"));
                        if let Some((fk, _)) = vocab::fact_key(&t.key)
                            && !fk.modes.contains(&m.kind.as_str())
                        {
                            ck.p(format!("{dp}.told_by.key"), "not observable in this mode");
                        }
                    }
                    check_tried(&mut ck, &dep.verified, &dp);
                }
            }
            None => {}
        }
        for (j, r) in m.restarts_into.iter().enumerate() {
            if d.mode(r).is_none() || *r == m.id {
                ck.p(format!("{path}.restarts_into[{j}]"), format!("no other mode {r:?}"));
            }
        }
        check_tried(&mut ck, &m.verified, &path);
    }

    // Transitions (FR-013).
    unique(&mut ck, d.transitions.iter().map(|t| t.id.as_str()), "transitions", "id");
    for (i, t) in d.transitions.iter().enumerate() {
        let path = format!("transitions[{i}]");
        for (k, v) in [("from", &t.from), ("to", &t.to)] {
            if d.mode(v).is_none() {
                ck.p(format!("{path}.{k}"), format!("no mode {v:?}"));
            }
        }
        if t.by == "installer"
            && let Some(m) = d.mode(&t.from)
            && !m.restarts_into.contains(&t.to)
        {
            ck.p(format!("{path}.by"), format!("mode {:?} can't restart into {:?}", t.from, t.to));
        }
        if t.by == "hand" && !d.texts.transitions.contains_key(&t.id) {
            ck.p(format!("texts.transitions.{}", t.id), "a by-hand transition needs its guide text");
        }
        check_tried(&mut ck, &t.verified, &path);
    }

    // Partition rules and boot settings (FR-014..FR-016a).
    let choice_ok = |w: &When| d.choice(&w.choice).is_some_and(|c| c.options.iter().any(|o| o.id == w.equals));
    for (i, r) in d.partitions.iter().enumerate() {
        let path = format!("partitions[{i}]");
        let destructive = matches!(r.class.as_str(), "resize" | "remove");
        match (destructive, r.destroys) {
            (true, None) => ck.p(
                format!("{path}.destroys"),
                "a resize or remove says whether it destroys what is on the partition (FR-015)",
            ),
            (false, Some(_)) => ck.p(format!("{path}.destroys"), "only a resize or remove destroys"),
            _ => {}
        }
        // From format 2 the whole-disk image backs up every partition (001 FR-009).
        if d.format < 2 && matches!(r.class.as_str(), "back-up" | "replace") && r.back_up.is_none() {
            ck.p(format!("{path}.back_up"), format!("a {} rule names how the backup is read (FR-016a)", r.class));
        }
        if r.class == "create" && r.back_up.is_some() {
            ck.p(format!("{path}.back_up"), "a partition the job creates has nothing to back up");
        }
        if let Some(b) = &r.back_up {
            check_back_up(&mut ck, b, &format!("{path}.back_up"));
        }
        if r.select.role.as_deref() == Some("disk") {
            match &r.back_up {
                Some(b) if b.with.is_none() => {}
                _ => ck.p(format!("{path}.back_up"), "a whole disk names how its image is read: mode, means and check"),
            }
            if r.when.is_some() {
                ck.p(format!("{path}.when"), "the whole-disk backup is the flow's choice, never a description's");
            }
        }
        if let Some(w) = &r.when
            && !choice_ok(w)
        {
            ck.p(format!("{path}.when"), format!("no choice {:?} with option {:?}", w.choice, w.equals));
        }
        if let Some(n) = &r.select.name
            && looks_unit_unique_name(n)
        {
            ck.p(format!("{path}.select.name"), "looks like a value unique to one unit");
        }
    }
    unique(&mut ck, d.boot_settings.iter().map(|b| b.id.as_str()), "boot_settings", "id");
    for (i, b) in d.boot_settings.iter().enumerate() {
        let path = format!("boot_settings[{i}]");
        if let Some(k) = vocab::BOOT_SETTING_KINDS.iter().find(|k| k.id == b.kind)
            && !k.backends.contains(&backend)
        {
            ck.p(
                format!("{path}.kind"),
                format!("the {backend} backend doesn't implement boot-setting kind {:?}", b.kind),
            );
        }
        check_back_up(&mut ck, &b.back_up, &format!("{path}.back_up"));
    }

    // What it keeps (FR-017, FR-018a) and writes (FR-019, FR-020).
    unique(&mut ck, d.unit_values.iter().map(|u| u.id.as_str()), "unit_values", "id");
    for (i, u) in d.unit_values.iter().enumerate() {
        if d.mode(&u.mode).is_none() {
            ck.p(format!("unit_values[{i}].mode"), format!("no mode {:?}", u.mode));
        }
    }
    unique(&mut ck, d.device_files.iter().map(|u| u.id.as_str()), "device_files", "id");
    check_device_data(&mut ck);
    check_supplied_files(&mut ck);
    unique(&mut ck, d.artifacts.iter().map(|a| a.role.as_str()), "artifacts", "role");
    for (i, c) in d.capabilities.iter().enumerate() {
        let path = format!("capabilities[{i}]");
        match (c.status.as_str(), &c.with) {
            ("needs", None) => ck.p(format!("{path}.with"), "a capability that needs something says what"),
            (s, Some(_)) if s != "needs" => ck.p(format!("{path}.with"), "only `needs` takes `with`"),
            _ => {}
        }
        if let Some(w) = &c.when
            && !choice_ok(w)
        {
            ck.p(format!("{path}.when"), format!("no choice {:?} with option {:?}", w.choice, w.equals));
        }
        check_tried(&mut ck, &c.verified, &path);
    }
    let mut seen_caps = BTreeSet::new();
    for (i, c) in d.capabilities.iter().enumerate() {
        let key = (c.id.as_str(), c.when.as_ref().map(|w| (w.choice.as_str(), w.equals.as_str())));
        if !seen_caps.insert(key) {
            ck.p(format!("capabilities[{i}].id"), format!("{:?} appears twice for the same choice", c.id));
        }
    }

    // Choices (FR-021).
    unique(&mut ck, d.choices.iter().map(|c| c.id.as_str()), "choices", "id");
    let named_partitions: BTreeSet<&str> = d
        .partitions
        .iter()
        .flat_map(|r| r.select.name.iter().chain(r.select.names.iter().flatten()))
        .map(String::as_str)
        .collect();
    for (i, c) in d.choices.iter().enumerate() {
        let path = format!("choices[{i}]");
        if crate::plan::RESERVED_CHOICES.contains(&c.id.as_str()) {
            ck.p(
                format!("{path}.id"),
                format!("{:?} is the flow's own choice (the backup, 001 FR-009a), never a description's", c.id),
            );
        }
        match c.source.as_str() {
            "fixed" => {
                if c.options.len() < 2 {
                    ck.p(format!("{path}.options"), "a choice has at least two outcomes");
                }
                unique(&mut ck, c.options.iter().map(|o| o.id.as_str()), &format!("{path}.options"), "id");
                match c.options.iter().find(|o| o.id == c.default) {
                    None => ck.p(format!("{path}.default"), format!("{:?} is not one of the options", c.default)),
                    Some(o) if o.replaces_system => ck.p(format!("{path}.default"), "replacing an operating system is never a default (FR-021)"),
                    _ => {}
                }
                for (j, o) in c.options.iter().enumerate() {
                    if named_partitions.contains(o.id.as_str()) {
                        ck.p(format!("{path}.options[{j}].id"), "a person chooses outcomes, never partitions");
                    }
                    if !d.texts.options.contains_key(&format!("{}.{}", c.id, o.id)) {
                        ck.p(format!("texts.options.\"{}.{}\"", c.id, o.id), "each option needs its text");
                    }
                }
            }
            _ => {
                if !c.options.is_empty() {
                    ck.p(format!("{path}.options"), "options come from inspecting");
                }
                if c.default != "first" {
                    ck.p(format!("{path}.default"), "for disks the default is `first`");
                }
            }
        }
        if c.for_each.is_some() && c.source != "fixed" {
            ck.p(format!("{path}.for_each"), "a template choice has fixed outcomes");
        }
        if !d.texts.choices.contains_key(&c.id) {
            ck.p(format!("texts.choices.{}", c.id), "each choice says what it means here (FR-026)");
        }
    }

    // Jobs and steps (FR-022..FR-024).
    unique(&mut ck, d.jobs.iter().map(|j| j.id.as_str()), "jobs", "id");
    for (i, j) in d.jobs.iter().enumerate() {
        for (k, s) in j.states.iter().enumerate() {
            if !d.states.iter().any(|x| x.id == *s) {
                ck.p(format!("jobs[{i}].states[{k}]"), format!("no state {s:?}"));
            }
        }
    }
    unique(&mut ck, d.steps.iter().map(|s| s.id.as_str()), "steps", "id");
    for (i, s) in d.steps.iter().enumerate() {
        check_step(&mut ck, i, s);
    }
    check_order(&mut ck);
    check_backed_up_first(&mut ck);
    if d.format >= 2 {
        check_disk_backup_first(&mut ck);
    }

    // Texts (FR-026).
    if d.texts.name.trim().is_empty() {
        ck.p("texts.name", "is empty");
    }
    let maps: [TextMap; 6] = [
        ("modes", &d.texts.modes, Box::new(|k| d.mode(k).is_some())),
        ("transitions", &d.texts.transitions, Box::new(|k| d.transition(k).is_some())),
        ("choices", &d.texts.choices, Box::new(|k| d.choice(k).is_some())),
        (
            "options",
            &d.texts.options,
            Box::new(|k| {
                k.split_once('.')
                    .is_some_and(|(c, o)| d.choice(c).is_some_and(|c| c.options.iter().any(|x| x.id == o)))
            }),
        ),
        ("states", &d.texts.states, Box::new(|k| d.states.iter().any(|s| s.id == k))),
        ("steps", &d.texts.steps, Box::new(|k| d.step(k).is_some())),
    ];
    for (name, map, exists) in &maps {
        for k in map.keys() {
            if !exists(k) {
                ck.p(
                    crate::shape::join(&format!("texts.{name}"), k),
                    format!("no {} {k:?}", name.trim_end_matches('s')),
                );
            }
        }
    }
    let mut texts: Vec<(String, &str)> = vec![("texts.name".into(), d.texts.name.as_str())];
    for (name, map, _) in &maps {
        texts.extend(map.iter().map(|(k, v)| (crate::shape::join(&format!("texts.{name}"), k), v.as_str())));
    }
    for (k, st) in &d.texts.supplied_files {
        let base = crate::shape::join("texts.supplied_files", k);
        if !d.supplied_files.iter().any(|f| f.what == *k) {
            ck.p(&base, format!("no supplied file's what is {k:?}"));
        }
        for (part, t) in [("what", &st.what), ("why", &st.why), ("recognise", &st.recognise)] {
            let path = format!("{base}.{part}");
            if t.trim().is_empty() {
                ck.p(&path, "is empty");
            }
            if looks_like_link(t) {
                ck.p(&path, "names no place to get the file: no links (FR-018b)");
            }
            texts.push((path, t.as_str()));
        }
    }
    for (path, t) in texts {
        if let Some(why) = markup(t) {
            ck.p(path, format!("plain text only, no markup ({why})"));
        }
    }
    ck.out
}

/// A string that looks like a link: a scheme or a `www.` host.
pub fn looks_like_link(t: &str) -> bool {
    let l = t.to_ascii_lowercase();
    l.contains("://") || l.contains("www.")
}

/// The supplied files (FR-018b): each needed by a mode that runs a supplied
/// program, its `matches` exactly the keys that mode's route checks, its
/// texts there (FR-026), and nothing in it that looks like a link: a
/// description never names a place to get a file.
fn check_supplied_files(ck: &mut Ck) {
    let d = ck.d;
    unique(ck, d.supplied_files.iter().map(|f| f.id.as_str()), "supplied_files", "id");
    for (i, f) in d.supplied_files.iter().enumerate() {
        let path = format!("supplied_files[{i}]");
        let mut strings: Vec<(String, &str)> = vec![
            (format!("{path}.id"), f.id.as_str()),
            (format!("{path}.what"), f.what.as_str()),
            (format!("{path}.usual_name"), f.usual_name.as_str()),
        ];
        strings.extend(f.matches.iter().map(|(k, v)| (format!("{path}.matches.{k}"), v.as_str())));
        for (j, k) in f.known.iter().enumerate() {
            let kp = format!("{path}.known[{j}]");
            strings.push((format!("{kp}.sha256"), k.sha256.as_str()));
            if let Some(u) = &k.untried {
                strings.push((format!("{kp}.untried"), u.as_str()));
            }
            if let Some(v) = &k.verified {
                strings.push((format!("{kp}.verified.record"), v.record.as_str()));
                strings.push((format!("{kp}.verified.boot_loader"), v.boot_loader.as_str()));
            }
        }
        if let Some(u) = &f.signature.untried {
            strings.push((format!("{path}.signature.untried"), u.as_str()));
        }
        for (p, v) in strings {
            if looks_like_link(v) {
                ck.p(p, "names no place to get the file: no links (FR-018b)");
            }
        }
        if f.usual_name.trim().is_empty() {
            ck.p(format!("{path}.usual_name"), "is empty");
        }
        if !d.texts.supplied_files.contains_key(&f.what) {
            ck.p(
                format!("{path}.what"),
                format!(
                    "no texts.supplied_files.{} (what it is, why GlassineOS can't supply it, how to recognise it)",
                    f.what
                ),
            );
        }
        unique(ck, f.known.iter().map(|k| k.sha256.as_str()), &format!("{path}.known"), "sha256");
        for (j, k) in f.known.iter().enumerate() {
            check_tried(ck, &k.verified, &format!("{path}.known[{j}]"));
        }
        check_tried(ck, &f.signature.verified, &format!("{path}.signature"));

        let needing: Vec<&format1::Mode> = d.modes.iter().filter(|m| m.needs.as_deref() == Some(f.id.as_str())).collect();
        if needing.is_empty() {
            ck.p(format!("{path}.id"), "no mode needs it");
        }
        let kinds: BTreeSet<&str> = needing.iter().map(|m| m.kind.as_str()).collect();
        for kind in kinds {
            let route = vocab::supplied_matches(kind);
            if route.is_empty() {
                continue; // the mode's `needs` says so
            }
            for m in &route {
                match f.matches.get(m.key) {
                    None => ck.p(format!("{path}.matches.{}", m.key), format!("the {kind} route checks {} (missing)", m.key)),
                    Some(v) if !regex_lite::Regex::new(m.pattern).is_ok_and(|r| r.is_match(v)) => {
                        ck.p(format!("{path}.matches.{}", m.key), format!("{v:?} does not have the form {}", m.pattern))
                    }
                    Some(_) => {}
                }
            }
            for k in f.matches.keys() {
                if !route.iter().any(|m| m.key == k) {
                    ck.p(format!("{path}.matches.{k}"), format!("the {kind} route doesn't check {k:?}"));
                }
            }
            // The boot ROM's facts the identity names agree with the file's (FR-018b).
            for (j, c) in d.identity.iter().enumerate() {
                for m in route.iter().filter(|m| m.fact_key == c.key) {
                    if let (Some(Scalar::Text(want)), Some(v)) = (&c.equals, f.matches.get(m.key))
                        && want != v
                    {
                        ck.p(format!("{path}.matches.{}", m.key), format!("{v:?} is not identity[{j}]'s {want:?}"));
                    }
                }
            }
        }
    }
}

/// The hashes a description may hold (format 3): a supplied file's known
/// checksums and the values its route checks it against, and the boot ROM's
/// key hash an identity signal or condition tests (FR-018b).
fn allowed_hashes(d: &Description) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    if d.format < 3 {
        return out;
    }
    for f in &d.supplied_files {
        out.extend(f.known.iter().map(|k| k.sha256.to_ascii_lowercase()));
        out.extend(f.matches.values().map(|v| v.to_ascii_lowercase()));
    }
    let keys: BTreeSet<&str> = vocab::SUPPLIED_MATCHES.iter().map(|m| m.fact_key).collect();
    for c in &d.identity {
        if keys.contains(c.key.as_str())
            && let Some(Scalar::Text(v)) = &c.equals
        {
            out.insert(v.to_ascii_lowercase());
        }
    }
    out
}

/// The device-data files (FR-018) as boot 006 FR-001..FR-005 reads them: each a
/// line of the list the installed system reads, so a description the check
/// accepts derives a list the seed accepts. Their sources are only read: a
/// partition a job may write, create, resize or remove is never one.
fn check_device_data(ck: &mut Ck) {
    let d = ck.d;
    let files: Vec<(usize, &format1::DeviceDataFile)> = d
        .device_data
        .iter()
        .enumerate()
        .filter_map(|(i, x)| match x {
            format1::DeviceDatum::File(f) => Some((i, f)),
            format1::DeviceDatum::Named(_) => None,
        })
        .collect();
    unique(ck, files.iter().map(|(_, f)| f.name.as_str()), "device_data", "name");
    for (i, f) in files {
        let path = format!("device_data[{i}]");
        // A name is relative, without `.` or `..` parts (FR-003); `manifest` is the seed's own.
        if f.name.split('/').any(|c| c == "." || c == "..") {
            ck.p(format!("{path}.name"), "a name has no . or .. parts (boot 006 FR-003)");
        }
        if f.name == "manifest" {
            ck.p(format!("{path}.name"), "manifest is the seed's own file");
        }
        if f.path.split('/').skip(1).any(|c| c == "." || c == "..") {
            ck.p(format!("{path}.path"), "a path has no . or .. parts");
        }
        // A split image by one pattern, NAME.* from PATH.*, on both sides or neither (FR-004).
        for (key, v) in [("name", &f.name), ("path", &f.path)] {
            if v.trim_end_matches(".*").contains('*') {
                ck.p(format!("{path}.{key}"), "* only as a pattern's last part, .*");
            }
        }
        if f.name.ends_with(".*") != f.path.ends_with(".*") {
            ck.p(format!("{path}.path"), "a pattern on both sides or neither (boot 006 FR-004)");
        }
        let classes = deciding_classes(d, &f.partition, &[]);
        let named = d.partitions.iter().any(|r| selects_ref(&r.select, &f.partition));
        if !named {
            ck.p(format!("{path}.partition"), format!("no partition rule selects {:?}", f.partition));
        } else if classes.iter().any(|c| c != "never-write") {
            ck.p(
                format!("{path}.partition"),
                format!(
                    "device data is read from partitions no job changes; {:?} may be {}",
                    f.partition,
                    classes.into_iter().collect::<Vec<_>>().join(", ")
                ),
            );
        }
    }
    if d.format >= 2 && d.device_data.iter().any(|x| matches!(x, format1::DeviceDatum::Named(_))) {
        // The shape rejects it first; this keeps the records honest.
        ck.p("device_data", "from format 2 each device-data file is a table (FR-018)");
    }
}

fn looks_unit_unique_name(n: &str) -> bool {
    vocab::looks_unit_unique(n)
}

/// The test fits the fact's type; the key is in the vocabulary and is no
/// unit-unique value.
fn check_test(ck: &mut Ck, c: &Condition, path: &str) {
    let Some((fk, param)) = vocab::fact_key_in(&c.key, ck.d.format) else {
        ck.p(format!("{path}.key"), format!("unknown fact key {:?} (FR-025)", c.key));
        return;
    };
    if vocab::looks_unit_unique(&c.key) {
        ck.p(format!("{path}.key"), "reads a value unique to one unit; a description never holds those");
    }
    if let Some(p) = &param
        && p.contains(['<', '>'])
    {
        ck.p(format!("{path}.key"), "fill in the key's <name>");
    }
    let test = c.test_name();
    if matches!(test, "present" | "absent") && c.present.or(c.absent) != Some(true) {
        ck.p(format!("{path}.{test}"), "write `true`");
    }
    let fits = match fk.ty {
        FactType::Presence => matches!(test, "present" | "absent"),
        FactType::Text => match test {
            "equals" => matches!(c.equals, Some(Scalar::Text(_))),
            "one-of" => c.one_of.as_ref().is_some_and(|v| v.iter().all(|x| matches!(x, Scalar::Text(_)))),
            "present" | "absent" => true,
            _ => false,
        },
        FactType::Number => match test {
            "equals" => matches!(c.equals, Some(Scalar::Int(_))),
            "one-of" => c.one_of.as_ref().is_some_and(|v| v.iter().all(|x| matches!(x, Scalar::Int(_)))),
            _ => true,
        },
    };
    if !fits {
        ck.p(format!("{path}.{test}"), format!("the test {test} doesn't fit {:?}, a {:?} fact", c.key, fk.ty));
    }
}

/// A condition of a state or a `done`: also its mode, which must observe it.
fn check_moded(ck: &mut Ck, c: &Condition, path: &str) {
    check_test(ck, c, path);
    let Some(mode) = &c.mode else { return };
    let kind = if mode == vocab::INSTALLER_MODE {
        Some(vocab::INSTALLER_MODE)
    } else {
        ck.d.mode(mode).map(|m| m.kind.as_str())
    };
    match kind {
        None => ck.p(format!("{path}.mode"), format!("no mode {mode:?}")),
        Some(k) => {
            if let Some((fk, _)) = vocab::fact_key(&c.key)
                && !fk.modes.contains(&k)
            {
                ck.p(format!("{path}.mode"), format!("{:?} can't be observed in a mode of kind {k:?}", c.key));
            }
        }
    }
    check_tried(ck, &c.verified, path);
}

fn check_tried(ck: &mut Ck, v: &Option<format1::Verified>, path: &str) {
    if let Some(v) = v {
        if !ck.d.states.iter().any(|s| s.id == v.state) {
            ck.p(format!("{path}.verified.state"), format!("no state {:?}", v.state));
        }
        if v.boot_loader.trim().is_empty() {
            ck.p(format!("{path}.verified.boot_loader"), "says the boot loader the run was made on");
        }
    }
}

fn check_back_up(ck: &mut Ck, b: &format1::BackUp, path: &str) {
    let d = ck.d;
    if let Some(w) = &b.with {
        if b.mode.is_some() || b.check.is_some() || b.size.is_some() {
            ck.p(path, "`with` takes nothing else: the other backup says how");
        }
        let covered = d
            .partitions
            .iter()
            .any(|r| r.back_up.as_ref().is_some_and(|x| x.with.is_none()) && selects_ref(&r.select, w));
        if !covered {
            ck.p(format!("{path}.with"), format!("no rule backs up {w:?} itself"));
        }
        return;
    }
    let (Some(mode), Some(means)) = (&b.mode, &b.means) else {
        ck.p(path, "needs mode, means and check (or with)");
        return;
    };
    if b.check.is_none() {
        ck.p(format!("{path}.check"), "missing required key");
    }
    match d.mode(mode) {
        None => ck.p(format!("{path}.mode"), format!("no mode {mode:?}")),
        Some(m) => {
            if let Some(k) = vocab::BACKUP_MEANS.iter().find(|k| k.id == *means) {
                if !k.modes.contains(&m.kind.as_str()) {
                    ck.p(format!("{path}.means"), format!("{means:?} doesn't work in a mode of kind {:?}", m.kind));
                }
                if !k.backends.contains(&d.backend.as_str()) {
                    ck.p(format!("{path}.means"), format!("the {} backend doesn't implement {means:?}", d.backend));
                }
            }
            if !m.can.iter().any(|c| c == "read") && m.untried.is_none() {
                ck.p(format!("{path}.mode"), format!("mode {mode:?} can't read"));
            }
        }
    }
}

/// Whether a rule's selector selects a partition reference statically: names
/// and patterns for names, roles for `role:` references. Type selectors only
/// decide at plan time, against the partitions inspecting found.
pub fn selects_ref(sel: &format1::Select, r: &str) -> bool {
    if let Some(role) = r.strip_prefix("role:") {
        return sel.role.as_deref() == Some(role);
    }
    sel.name.as_deref() == Some(r) || sel.names.as_ref().is_some_and(|n| n.iter().any(|x| x == r)) || sel.pattern.as_deref().is_some_and(|p| glob(p, r))
}

pub fn glob(pat: &str, s: &str) -> bool {
    match pat.split_once('*') {
        None => pat == s,
        Some((pre, rest)) => {
            if !s.starts_with(pre) {
                return false;
            }
            let s = &s[pre.len()..];
            (0..=s.len()).any(|i| s.is_char_boundary(i) && glob(rest, &s[i..]))
        }
    }
}

/// The classes that may decide a reference for a step, given the step's own
/// choices: a rule whose `when` the step's choices don't settle may or may not
/// apply, so both branches count.
pub fn deciding_classes(d: &Description, r: &str, when: &[When]) -> BTreeSet<String> {
    fn go(d: &Description, r: &str, when: &[When], from: usize, out: &mut BTreeSet<String>) {
        for (i, rule) in d.partitions.iter().enumerate().skip(from) {
            if !selects_ref(&rule.select, r) {
                continue;
            }
            match &rule.when {
                None => {
                    out.insert(rule.class.clone());
                    return;
                }
                Some(w) => match when.iter().find(|x| x.choice == w.choice) {
                    Some(x) if x.equals == w.equals => {
                        out.insert(rule.class.clone());
                        return;
                    }
                    Some(_) => continue,
                    None => {
                        out.insert(rule.class.clone());
                        go(d, r, when, i + 1, out);
                        return;
                    }
                },
            }
        }
        out.insert("never-write".into());
    }
    let mut out = BTreeSet::new();
    go(d, r, when, 0, &mut out);
    out
}

fn check_step(ck: &mut Ck, i: usize, s: &Step) {
    let d = ck.d;
    let path = format!("steps[{i}]");
    if !d.jobs.iter().any(|j| j.id == s.job) {
        ck.p(format!("{path}.job"), format!("no job {:?} in jobs", s.job));
    }
    let mode = d.mode(&s.mode);
    if mode.is_none() {
        ck.p(format!("{path}.mode"), format!("no mode {:?}", s.mode));
    }
    if let Some(k) = vocab::step_kind(&s.kind) {
        if !k.backends.contains(&d.backend.as_str()) {
            ck.p(
                format!("{path}.kind"),
                format!("the {} backend doesn't implement step kind {:?} (FR-025)", d.backend, s.kind),
            );
        }
        if let Some(m) = mode
            && !k.modes.contains(&m.kind.as_str())
        {
            ck.p(format!("{path}.mode"), format!("a {:?} step can't run in a mode of kind {:?}", s.kind, m.kind));
        }
        for p in k.params {
            let Some(v) = s.params.get(p.name) else { continue };
            match (p.ty, v) {
                (vocab::ParamType::Artifact, toml::Value::String(role)) if !d.artifacts.iter().any(|a| a.role == *role) => {
                    ck.p(format!("{path}.params.{}", p.name), format!("no artifact with role {role:?}"));
                }
                (vocab::ParamType::Partition, toml::Value::String(part)) if !s.writes.contains(part) && !s.creates.contains(part) => {
                    ck.p(format!("{path}.params.{}", p.name), format!("{part:?} is not in the step's writes or creates"));
                }
                (vocab::ParamType::NewPartitions, toml::Value::Array(news)) if d.format >= 2 => {
                    for (j, n) in news.iter().enumerate() {
                        check_new_partition(ck, s, n, &format!("{path}.params.{}[{j}]", p.name));
                    }
                }
                _ => {}
            }
        }
    }
    if let Some(m) = mode
        && s.writes_anything()
        && !m.can.iter().any(|c| c == "write")
    {
        ck.p(format!("{path}.mode"), format!("mode {:?} can't write", s.mode));
    }
    for (j, w) in s.when.iter().enumerate() {
        if !d.choice(&w.choice).is_some_and(|c| c.options.iter().any(|o| o.id == w.equals)) {
            ck.p(format!("{path}.when[{j}]"), format!("no choice {:?} with option {:?}", w.choice, w.equals));
        }
    }
    if let Some(fe) = &s.for_each
        && !d.choices.iter().any(|c| c.for_each.as_deref() == Some(fe.as_str()))
    {
        ck.p(format!("{path}.for_each"), format!("no template choice for {fe:?}"));
    }
    for (action, list) in s.actions() {
        for (j, r) in list.iter().enumerate() {
            let rp = format!("{path}.{action}[{j}]");
            if let Some(role) = r.strip_prefix("role:")
                && !vocab::ids_in(vocab::ROLES, d.format).contains(&role)
            {
                ck.p(&rp, format!("unknown role {role:?}"));
                continue;
            }
            for class in deciding_classes(d, r, &s.when) {
                if !vocab::class_allows(&class, action) {
                    ck.p(
                        &rp,
                        format!("goes beyond its partition's rule: {r:?} is {class}, which doesn't allow {action} (FR-014a)"),
                    );
                }
            }
        }
    }
    for (j, b) in s.boot_settings.iter().enumerate() {
        if !d.boot_settings.iter().any(|x| x.id == *b) {
            ck.p(format!("{path}.boot_settings[{j}]"), format!("no boot setting {b:?}"));
        }
    }
    for (j, c) in s.done.iter().enumerate() {
        check_moded(ck, c, &format!("{path}.done[{j}]"));
    }
    for (case, e) in [("failure", &s.escape.failure), ("interruption", &s.escape.interruption)] {
        let ep = format!("{path}.escape.{case}");
        match (e.leaves.as_str(), &e.hardware) {
            ("special-hardware", None) => ck.p(format!("{ep}.hardware"), "say which special hardware"),
            (l, Some(_)) if l != "special-hardware" => ck.p(format!("{ep}.hardware"), "only `special-hardware` names hardware"),
            _ => {}
        }
        if let Some(st) = &e.way_out.step
            && d.step(st).is_none()
        {
            ck.p(format!("{ep}.way_out.step"), format!("no step {st:?}"));
        }
        if let Some(t) = &e.way_out.transition {
            match d.transition(t) {
                None => ck.p(format!("{ep}.way_out.transition"), format!("no transition {t:?}")),
                Some(t) => {
                    let special: Vec<&String> = t.needs.iter().filter(|n| vocab::SPECIAL_HARDWARE.contains(&n.as_str())).collect();
                    if !special.is_empty() && e.hardware.as_ref() != special.first().copied() {
                        ck.p(
                            format!("{ep}.leaves"),
                            format!("its way out needs {}; say so: leaves = special-hardware", special[0]),
                        );
                    }
                }
            }
        }
    }
    check_tried(ck, &s.verified, &path);
}

/// `after` references exist, stay in the job, and make no cycle.
/// One partition a step creates, with its size (FR-014a): it is in the step's
/// `creates`, a literal `at_least` goes only with a relative size, and
/// `release:<role>` names an artifact of the description (FR-019a).
fn check_new_partition(ck: &mut Ck, s: &Step, n: &toml::Value, path: &str) {
    let Some(t) = n.as_table() else { return };
    if let Some(toml::Value::String(name)) = t.get("name")
        && !s.creates.contains(name)
    {
        ck.p(format!("{path}.name"), format!("{name:?} is not in the step's creates"));
    }
    let fixed = match t.get("size") {
        Some(toml::Value::Integer(_)) => true,
        Some(toml::Value::String(x)) => regex_lite::Regex::new(format1::FIXED_SIZE_PATTERN).is_ok_and(|r| r.is_match(x)),
        _ => false,
    };
    match t.get("at_least") {
        Some(toml::Value::String(x)) if x.starts_with("release:") => {
            let role = &x["release:".len()..];
            if !ck.d.artifacts.iter().any(|a| a.role == role) {
                ck.p(format!("{path}.at_least"), format!("no artifact with role {role:?}"));
            }
        }
        Some(_) if fixed => ck.p(format!("{path}.at_least"), "a fixed size is its own least: only `release:<role>` goes with it"),
        _ => {}
    }
}

fn check_order(ck: &mut Ck) {
    let d = ck.d;
    for (i, s) in d.steps.iter().enumerate() {
        for (j, a) in s.after.iter().enumerate() {
            match d.step(a) {
                None => ck.p(format!("steps[{i}].after[{j}]"), format!("no step {a:?}")),
                Some(x) if x.job != s.job => ck.p(format!("steps[{i}].after[{j}]"), format!("{a:?} is a step of another job")),
                _ => {}
            }
        }
    }
    for (i, s) in d.steps.iter().enumerate() {
        let mut seen = BTreeSet::new();
        let mut todo: Vec<&str> = s.after.iter().map(String::as_str).collect();
        while let Some(a) = todo.pop() {
            if a == s.id {
                ck.p(format!("steps[{i}].after"), "the order has a cycle");
                break;
            }
            if seen.insert(a)
                && let Some(x) = d.step(a)
            {
                todo.extend(x.after.iter().map(String::as_str));
            }
        }
    }
}

/// The steps a step comes after, directly or through others.
fn steps_before(d: &Description, s: &Step) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut todo: Vec<&str> = s.after.iter().map(String::as_str).collect();
    while let Some(a) = todo.pop() {
        if seen.insert(a.to_string())
            && let Some(x) = d.step(a)
        {
            todo.extend(x.after.iter().map(String::as_str));
        }
    }
    seen
}

/// From format 2: every job that writes has a back-up step that images
/// whole disks (`backs_up = ["role:disk"]`, 001 FR-009), and every step that
/// writes comes after it. The plan leaves it out only when the person
/// switches the backup off (FR-009a), never because a description does.
fn check_disk_backup_first(ck: &mut Ck) {
    let d = ck.d;
    let images = |s: &Step| s.kind == "back-up" && s.backs_up.iter().any(|r| r == "role:disk") && s.when.is_empty();
    let has_rule = d
        .partitions
        .iter()
        .any(|r| r.select.role.as_deref() == Some("disk") && r.back_up.as_ref().is_some_and(|b| b.with.is_none()));
    for j in &d.jobs {
        let writing: Vec<(usize, &Step)> = d.steps.iter().enumerate().filter(|(_, s)| s.job == j.id && s.writes_anything()).collect();
        if writing.is_empty() {
            continue;
        }
        if !has_rule {
            ck.p(
                "partitions",
                "a description whose jobs write has a rule for role disk that says how a whole disk is read (001 FR-009)",
            );
        }
        let imaging: Vec<&str> = d.steps.iter().filter(|s| s.job == j.id && images(s)).map(|s| s.id.as_str()).collect();
        if imaging.is_empty() {
            ck.p(
                "steps",
                format!("job {:?} writes, but no back-up step of it backs up role:disk first (001 FR-009)", j.id),
            );
            continue;
        }
        for (i, s) in writing {
            let earlier = steps_before(d, s);
            if !imaging.iter().any(|x| earlier.contains(*x)) {
                ck.p(
                    format!("steps[{i}].after"),
                    format!("{:?} writes before the whole-disk backup ({})", s.id, imaging.join(", ")),
                );
            }
        }
    }
}

/// Everything a step replaces (and each boot setting it changes) is backed up
/// by an earlier step of the same job (001 FR-009).
fn check_backed_up_first(ck: &mut Ck) {
    let d = ck.d;
    let before = |s: &Step| steps_before(d, s);
    let backup_of = |r: &str| -> String {
        d.partitions
            .iter()
            .find(|rule| selects_ref(&rule.select, r))
            .and_then(|rule| rule.back_up.as_ref())
            .and_then(|b| b.with.clone())
            .unwrap_or_else(|| r.to_string())
    };
    for (i, s) in d.steps.iter().enumerate() {
        let earlier = before(s);
        let backed: BTreeSet<&str> = d
            .steps
            .iter()
            .filter(|x| earlier.contains(&x.id))
            .flat_map(|x| x.backs_up.iter().chain(if x.kind == "back-up" { x.boot_settings.iter() } else { [].iter() }))
            .map(String::as_str)
            .collect();
        for (j, w) in s.writes.iter().enumerate() {
            if s.creates.contains(w) || d.steps.iter().any(|x| earlier.contains(&x.id) && x.creates.contains(w)) {
                continue;
            }
            if deciding_classes(d, w, &s.when).iter().all(|c| c != "create")
                && !backed.contains(w.as_str())
                && !backed.contains(backup_of(w).as_str())
                && !backed.contains("role:disk")
            {
                ck.p(
                    format!("steps[{i}].writes[{j}]"),
                    format!("{w:?} is replaced before an earlier step backs it up"),
                );
            }
        }
        if s.kind != "back-up" {
            for (j, b) in s.boot_settings.iter().enumerate() {
                let via = d.boot_settings.iter().find(|x| x.id == *b).and_then(|x| x.back_up.with.clone());
                if !backed.contains(b.as_str()) && !via.as_deref().is_some_and(|v| backed.contains(v)) {
                    ck.p(
                        format!("steps[{i}].boot_settings[{j}]"),
                        format!("boot setting {b:?} is changed before an earlier step backs it up"),
                    );
                }
            }
        }
    }
}

/// Why a text isn't plain, if it isn't.
pub fn markup(t: &str) -> Option<&'static str> {
    let tag = regex_lite::Regex::new(r"</?[A-Za-z][^>]*>").unwrap();
    if tag.is_match(t) {
        return Some("an HTML tag");
    }
    if t.contains("](") {
        return Some("a Markdown link");
    }
    if t.contains('`') || t.contains("**") || t.contains("__") {
        return Some("Markdown emphasis or code");
    }
    if regex_lite::Regex::new(r"&[a-z]+;|&#[0-9]+;").unwrap().is_match(t) {
        return Some("an HTML entity");
    }
    if t.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with('#') || l.starts_with("- ") || l.starts_with("* ") || l.starts_with("> ")
    }) {
        return Some("a Markdown heading, list or quote");
    }
    None
}

/// What a description never holds, as far as the text shows it: hashes or
/// unit values written out (long runs of hex digits).
pub fn never_holds(text: &str) -> Vec<Problem> {
    never_holds_except(text, &BTreeSet::new())
}

/// [`never_holds`], but a run of hex digits that is one of `allowed` (lower
/// case), as a whole, is not a problem: the checksums and key hashes format 3
/// lets a supplied file hold (FR-018b, [`allowed_hashes`]).
pub fn never_holds_except(text: &str, allowed: &BTreeSet<String>) -> Vec<Problem> {
    let hex = regex_lite::Regex::new(r"[0-9a-fA-F]{32,}").unwrap();
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if hex.find_iter(line).any(|m| !allowed.contains(&m.as_str().to_ascii_lowercase())) {
            out.push(problem(
                &format!("line {}", n + 1),
                "holds a long hex string: a description never holds hashes, keys or unit values",
            ));
        }
    }
    out
}

/// Rules across descriptions: ids unique, `refines` names one that exists, of
/// the same backend, with no cycle (FR-005, FR-007a).
pub fn check_set(ds: &[&Description]) -> Vec<(String, Problem)> {
    let mut out = Vec::new();
    let mut ids = BTreeSet::new();
    for d in ds {
        if !ids.insert(d.id.as_str()) {
            out.push((d.id.clone(), problem("id", format!("{:?} is the id of another description", d.id))));
        }
    }
    let by_id: BTreeMap<&str, &&Description> = ds.iter().map(|d| (d.id.as_str(), d)).collect();
    for d in ds {
        let Some(r) = &d.refines else { continue };
        match by_id.get(r.as_str()) {
            None => out.push((d.id.clone(), problem("refines", format!("no description {r:?}")))),
            Some(p) if p.backend != d.backend => out.push((d.id.clone(), problem("refines", format!("{r:?} has another backend")))),
            Some(_) => {
                let mut seen = BTreeSet::new();
                let mut cur = Some(r.as_str());
                while let Some(c) = cur {
                    if c == d.id || !seen.insert(c) {
                        out.push((d.id.clone(), problem("refines", "the refinement has a cycle")));
                        break;
                    }
                    cur = by_id.get(c).and_then(|x| x.refines.as_deref());
                }
            }
        }
    }
    out
}
