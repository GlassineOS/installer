// SPDX-License-Identifier: GPL-2.0-or-later
//! Format 1 of the device description (spec 002): its structure as a shape
//! (for the walker and the schema) and as typed records (for everything after).

use crate::shape::*;
use crate::vocab::{self, ParamType};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const FORMAT: i64 = 1;

pub const ID_PATTERN: &str = "^[a-z0-9]+(-[a-z0-9]+)*$";
/// Partition names as the device's table has them, or `role:<role>`.
pub const PART_REF_PATTERN: &str = "^(role:[a-z-]+|[A-Za-z0-9_.-]+)$";
pub const SIZE_PATTERN: &str = "^([0-9]+(KiB|MiB|GiB|TiB)|rest-of-free-space|rest-of-disk|unknown|after:[A-Za-z0-9_.-]+)$";
pub const RECORD_PATTERN: &str = "^([A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+#[0-9]+|https://docs\\.glassine\\.org/[A-Za-z0-9_./#-]*)$";
pub const USB_ID_PATTERN: &str = "^[0-9a-f]{4}$";
pub const USB_IF_PATTERN: &str = "^[0-9a-f]{2}-[0-9a-f]{2}-[0-9a-f]{2}$";

// ---- typed records ------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Scalar {
    Int(i64),
    Text(String),
}

impl std::fmt::Display for Scalar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Scalar::Int(i) => write!(f, "{i}"),
            Scalar::Text(s) => write!(f, "{s:?}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Verified {
    pub record: String,
    pub state: String,
    pub boot_loader: String,
}

/// One test of one fact: an identity signal (FR-006), a state's condition or a
/// step's `done` (FR-009, FR-022), a departure's first answer (FR-011).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    pub key: String,
    #[serde(default)]
    pub equals: Option<Scalar>,
    #[serde(default, rename = "one-of")]
    pub one_of: Option<Vec<Scalar>>,
    #[serde(default)]
    pub present: Option<bool>,
    #[serde(default)]
    pub absent: Option<bool>,
    #[serde(default, rename = "at-least")]
    pub at_least: Option<i64>,
    #[serde(default, rename = "at-most")]
    pub at_most: Option<i64>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub required: Option<bool>,
    #[serde(default)]
    pub verified: Option<Verified>,
    #[serde(default)]
    pub untried: Option<String>,
}

impl Condition {
    pub fn test_name(&self) -> &'static str {
        if self.equals.is_some() {
            "equals"
        } else if self.one_of.is_some() {
            "one-of"
        } else if self.present.is_some() {
            "present"
        } else if self.absent.is_some() {
            "absent"
        } else if self.at_least.is_some() {
            "at-least"
        } else {
            "at-most"
        }
    }

    /// The test in words, for plans and reports.
    pub fn describe(&self) -> String {
        if let Some(v) = &self.equals {
            format!("{} = {v}", self.key)
        } else if let Some(vs) = &self.one_of {
            format!("{} one of {}", self.key, vs.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", "))
        } else if self.present.is_some() {
            format!("{} present", self.key)
        } else if self.absent.is_some() {
            format!("{} absent", self.key)
        } else if let Some(n) = self.at_least {
            format!("{} at least {n}", self.key)
        } else {
            format!("{} at most {}", self.key, self.at_most.unwrap_or_default())
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub id: String,
    pub conditions: Vec<Condition>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usb {
    pub vendor_id: String,
    pub product_id: String,
    #[serde(default)]
    pub interface: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Departure {
    pub kind: String,
    #[serde(default)]
    pub params: toml::Table,
    #[serde(default)]
    pub told_by: Option<Condition>,
    #[serde(default)]
    pub verified: Option<Verified>,
    #[serde(default)]
    pub untried: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Mode {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub usb: Option<Usb>,
    pub can: Vec<String>,
    #[serde(default)]
    pub restarts_into: Vec<String>,
    #[serde(default)]
    pub departures: Vec<Departure>,
    #[serde(default)]
    pub verified: Option<Verified>,
    #[serde(default)]
    pub untried: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transition {
    pub id: String,
    pub from: String,
    pub to: String,
    pub by: String,
    #[serde(default)]
    pub needs: Vec<String>,
    #[serde(default)]
    pub verified: Option<Verified>,
    #[serde(default)]
    pub untried: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Select {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub names: Option<Vec<String>>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default, rename = "type")]
    pub type_guid: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct When {
    pub choice: String,
    pub equals: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackUp {
    /// Backed up as part of another backup (a partition or `role:...`).
    #[serde(default)]
    pub with: Option<String>,
    #[serde(default)]
    pub mode: Option<String>,
    #[serde(default)]
    pub means: Option<String>,
    #[serde(default)]
    pub check: Option<String>,
    #[serde(default)]
    pub size: Option<Scalar>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartitionRule {
    pub select: Select,
    pub class: String,
    #[serde(default)]
    pub destroys: Option<bool>,
    #[serde(default)]
    pub when: Option<When>,
    #[serde(default)]
    pub back_up: Option<BackUp>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootSetting {
    pub id: String,
    pub kind: String,
    pub back_up: BackUp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnitValue {
    pub id: String,
    pub what: String,
    pub mode: String,
    pub means: String,
    pub goes_to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceFile {
    pub id: String,
    pub what: String,
    #[serde(rename = "where")]
    pub location: String,
    pub used_by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub role: String,
    pub size: Scalar,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityEntry {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub with: Option<String>,
    #[serde(default)]
    pub when: Option<When>,
    #[serde(default)]
    pub verified: Option<Verified>,
    #[serde(default)]
    pub untried: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChoiceOption {
    pub id: String,
    #[serde(default)]
    pub replaces_system: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    pub id: String,
    pub source: String,
    #[serde(default)]
    pub for_each: Option<String>,
    #[serde(default)]
    pub options: Vec<ChoiceOption>,
    pub default: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub id: String,
    pub states: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WayOut {
    #[serde(default)]
    pub step: Option<String>,
    #[serde(default)]
    pub transition: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EscapeCase {
    pub leaves: String,
    #[serde(default)]
    pub hardware: Option<String>,
    pub way_out: WayOut,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Escape {
    pub failure: EscapeCase,
    pub interruption: EscapeCase,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    pub job: String,
    pub kind: String,
    #[serde(default)]
    pub params: toml::Table,
    pub mode: String,
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default)]
    pub when: Vec<When>,
    #[serde(default)]
    pub for_each: Option<String>,
    #[serde(default)]
    pub reads: Vec<String>,
    #[serde(default)]
    pub backs_up: Vec<String>,
    #[serde(default)]
    pub writes: Vec<String>,
    #[serde(default)]
    pub creates: Vec<String>,
    #[serde(default)]
    pub resizes: Vec<String>,
    #[serde(default)]
    pub removes: Vec<String>,
    #[serde(default)]
    pub boot_settings: Vec<String>,
    pub done: Vec<Condition>,
    pub escape: Escape,
    pub point_of_no_return: bool,
    pub safe_stop: bool,
    #[serde(default)]
    pub verified: Option<Verified>,
    #[serde(default)]
    pub untried: Option<String>,
}

impl Step {
    /// The partition actions in the vocabulary's order.
    pub fn actions(&self) -> [(&'static str, &Vec<String>); 6] {
        [
            ("reads", &self.reads),
            ("backs_up", &self.backs_up),
            ("writes", &self.writes),
            ("creates", &self.creates),
            ("resizes", &self.resizes),
            ("removes", &self.removes),
        ]
    }

    pub fn writes_anything(&self) -> bool {
        // A back-up step names the boot settings it backs up, not ones it changes.
        let boot = !self.boot_settings.is_empty() && self.kind != "back-up";
        boot || !(self.writes.is_empty() && self.creates.is_empty() && self.resizes.is_empty() && self.removes.is_empty())
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Texts {
    pub name: String,
    #[serde(default)]
    pub modes: BTreeMap<String, String>,
    #[serde(default)]
    pub transitions: BTreeMap<String, String>,
    #[serde(default)]
    pub choices: BTreeMap<String, String>,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
    #[serde(default)]
    pub states: BTreeMap<String, String>,
    #[serde(default)]
    pub steps: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Description {
    pub format: i64,
    pub id: String,
    pub name: String,
    pub backend: String,
    #[serde(default)]
    pub generic: bool,
    #[serde(default)]
    pub refines: Option<String>,
    #[serde(default)]
    pub device_data: Vec<String>,
    pub identity: Vec<Condition>,
    pub states: Vec<State>,
    pub modes: Vec<Mode>,
    #[serde(default)]
    pub transitions: Vec<Transition>,
    pub partitions: Vec<PartitionRule>,
    #[serde(default)]
    pub boot_settings: Vec<BootSetting>,
    #[serde(default)]
    pub unit_values: Vec<UnitValue>,
    #[serde(default)]
    pub device_files: Vec<DeviceFile>,
    #[serde(default)]
    pub artifacts: Vec<Artifact>,
    #[serde(default)]
    pub capabilities: Vec<CapabilityEntry>,
    #[serde(default)]
    pub choices: Vec<Choice>,
    pub jobs: Vec<Job>,
    pub steps: Vec<Step>,
    pub texts: Texts,
}

impl Description {
    pub fn mode(&self, id: &str) -> Option<&Mode> {
        self.modes.iter().find(|m| m.id == id)
    }
    pub fn step(&self, id: &str) -> Option<&Step> {
        self.steps.iter().find(|s| s.id == id)
    }
    pub fn choice(&self, id: &str) -> Option<&Choice> {
        self.choices.iter().find(|c| c.id == id)
    }
    pub fn transition(&self, id: &str) -> Option<&Transition> {
        self.transitions.iter().find(|t| t.id == id)
    }
}

// ---- the shape ------------------------------------------------------------

fn tried(mut fields: Vec<Field>) -> Vec<Field> {
    fields.push(opt("verified", verified()));
    fields.push(opt("untried", s("not tried on real hardware yet: why, or what is missing")));
    fields
}

fn verified() -> Shape {
    table(Table::new(
        "a recorded run on real hardware (FR-023a)",
        vec![
            f("record", sp(RECORD_PATTERN, "the record: owner/repo#number, or a page on docs.glassine.org")),
            f("state", sp(ID_PATTERN, "the install state the run was made in")),
            f("boot_loader", s("the boot loader the run was made on")),
        ],
    ))
}

fn tests() -> Vec<Field> {
    vec![
        opt(
            "equals",
            Shape::IntOrStr {
                pattern: "^.*$",
                doc: "the fact has this value".into(),
            },
        ),
        opt(
            "one-of",
            arr1(
                Shape::IntOrStr {
                    pattern: "^.*$",
                    doc: String::new(),
                },
                "the fact has one of these values",
            ),
        ),
        opt(
            "present",
            Shape::Str {
                pattern: None,
                values: None,
                doc: String::new(),
            },
        ),
        opt(
            "absent",
            Shape::Str {
                pattern: None,
                values: None,
                doc: String::new(),
            },
        ),
        opt("at-least", int("the number is at least this")),
        opt("at-most", int("the number is at most this")),
    ]
}

/// A test of one fact. `with_mode`: conditions of states and `done` name the
/// mode they are observed in; `identity`: signals say whether they are required
/// and whether they were seen on hardware.
fn condition(doc: &str, with_mode: bool, identity: bool) -> Shape {
    let mut fields = vec![f("key", sp("^[a-z]+\\.[A-Za-z0-9_.:<>-]+$", "the fact key (see the fact vocabulary)"))];
    for mut t in tests() {
        t.shape = match t.name {
            "present" | "absent" => Shape::True {
                doc: format!("the fact is {} (write true)", t.name),
            },
            _ => t.shape,
        };
        fields.push(t);
    }
    if with_mode {
        fields.push(f(
            "mode",
            sp(ID_PATTERN, "the mode it is observed in, or `installer` for the installer's own records"),
        ));
        fields = tried(fields);
    }
    if identity {
        fields.push(f("required", boolean("required (all must hold for a match) or supporting")));
        fields = tried(fields);
    }
    let mut t = Table::new(doc, fields).one_of(&["equals", "one-of", "present", "absent", "at-least", "at-most"]);
    if identity {
        t = t.one_of(&["verified", "untried"]);
    }
    table(t)
}

fn params_table(id: &str, params: &[vocab::Param]) -> (String, Table) {
    let fields = params
        .iter()
        .map(|p| Field {
            name: p.name,
            shape: param_shape(p.ty, p.doc),
            required: p.required,
        })
        .collect();
    (id.to_string(), Table::new(format!("parameters of {id}"), fields))
}

fn param_shape(ty: ParamType, doc: &str) -> Shape {
    match ty {
        ParamType::Text => s(doc),
        ParamType::Number => uint(doc),
        ParamType::Bool => boolean(doc),
        ParamType::Size => Shape::IntOrStr {
            pattern: SIZE_PATTERN,
            doc: doc.into(),
        },
        ParamType::Artifact => se(&vocab::ids(vocab::ARTIFACT_ROLES), doc),
        ParamType::Partition => sp(PART_REF_PATTERN, doc),
        ParamType::Slot => sp("^[a-z]$", doc),
        ParamType::Renames => Shape::Map {
            key_pattern: Some("^[A-Za-z0-9_.-]+$"),
            keys: None,
            value: Box::new(sp("^[A-Za-z0-9_.-]+$", "")),
            doc: doc.into(),
        },
        ParamType::NewPartitions => arr1(
            table(Table::new(
                "a partition to create",
                vec![
                    f("name", sp("^[a-z0-9-]+$", "the partition's name (GlassineOS's own)")),
                    f(
                        "size",
                        Shape::IntOrStr {
                            pattern: SIZE_PATTERN,
                            doc: "bytes, or relative".into(),
                        },
                    ),
                    opt("role", se(&["esp", "system"], "what it is for")),
                ],
            )),
            doc,
        ),
        ParamType::Operation => s(doc),
    }
}

fn tagged(t: Table, tag: &'static str, kinds: &[vocab::Kind]) -> Table {
    let variants = kinds.iter().map(|k| params_table(k.id, k.params)).collect();
    Table {
        tagged: Some(Tagged { tag, sub: "params", variants }),
        ..t
    }
}

fn when() -> Shape {
    table(Table::new(
        "applies only when a choice has this outcome",
        vec![f("choice", sp(ID_PATTERN, "the choice")), f("equals", sp(ID_PATTERN, "the option"))],
    ))
}

fn back_up() -> Shape {
    table(
        Table::new(
            "how it is backed up (FR-016a)",
            vec![
                opt(
                    "with",
                    sp(PART_REF_PATTERN, "backed up as part of this partition's backup (or role:partition-table)"),
                ),
                opt("mode", sp(ID_PATTERN, "the mode it is read in")),
                opt("means", se(&kinds_ids(vocab::BACKUP_MEANS), "how it is read")),
                opt("check", se(&vocab::ids(vocab::BACKUP_CHECKS), "how the backup is checked")),
                opt(
                    "size",
                    Shape::IntOrStr {
                        pattern: "^unknown$",
                        doc: "its size in bytes, when known".into(),
                    },
                ),
            ],
        )
        .one_of(&["with", "means"]),
    )
}

fn kinds_ids(k: &[vocab::Kind]) -> Vec<&'static str> {
    k.iter().map(|k| k.id).collect()
}

fn escape_case(doc: &str) -> Shape {
    table(Table::new(
        doc,
        vec![
            f("leaves", se(&vocab::ids(vocab::ESCAPE_LEAVES), "the state it leaves the device in")),
            opt("hardware", se(vocab::SPECIAL_HARDWARE, "the special hardware, when only that helps")),
            f(
                "way_out",
                table(
                    Table::new(
                        "the way out",
                        vec![opt("step", sp(ID_PATTERN, "another step")), opt("transition", sp(ID_PATTERN, "a transition"))],
                    )
                    .one_of(&["step", "transition"]),
                ),
            ),
        ],
    ))
}

fn text_map(doc: &str, key_pattern: &'static str) -> Shape {
    Shape::Map {
        key_pattern: Some(key_pattern),
        keys: None,
        value: Box::new(s("plain text, no markup")),
        doc: doc.into(),
    }
}

/// The whole of format 1.
pub fn shape() -> Table {
    let ids = |l: &[vocab::Entry]| vocab::ids(l);
    let part_list = |doc: &str| arr(sp(PART_REF_PATTERN, ""), doc);
    let mode_kinds: Vec<&str> = vocab::MODE_KINDS.iter().map(|m| m.id).collect();
    let cap_ids: Vec<&str> = vocab::CAPABILITIES.iter().map(|c| c.id).collect();

    let identity = condition("a signal of the device's identity (FR-006)", false, true);
    let state = table(Table::new(
        "an install state (FR-009)",
        vec![
            f("id", sp(ID_PATTERN, "stock, prepared, installed, or the description's own")),
            f("conditions", arr1(condition("a condition that tells the state", true, false), "")),
        ],
    ));
    let departure = table(
        tagged(
            Table::new(
                "a departure from the mode's standard protocol (FR-011)",
                tried(vec![
                    f("kind", se(&kinds_ids(vocab::DEPARTURES), "the departure")),
                    opt(
                        "told_by",
                        condition(
                            "the first answer that tells it, when it must be known before recognition (FR-012)",
                            false,
                            false,
                        ),
                    ),
                ]),
            ),
            "kind",
            vocab::DEPARTURES,
        )
        .one_of(&["verified", "untried"]),
    );
    let mode = table(
        Table::new(
            "a way the installer talks to the device (FR-010)",
            tried(vec![
                f("id", sp(ID_PATTERN, "")),
                f("kind", se(&mode_kinds, "the mode kind")),
                opt(
                    "usb",
                    table(Table::new(
                        "how it shows on USB",
                        vec![
                            f("vendor_id", sp(USB_ID_PATTERN, "")),
                            f("product_id", sp(USB_ID_PATTERN, "")),
                            opt("interface", sp(USB_IF_PATTERN, "class-subclass-protocol, hex")),
                        ],
                    )),
                ),
                f("can", arr(se(&ids(vocab::MODE_CAN), ""), "what it can do; empty while unknown")),
                opt("restarts_into", arr(sp(ID_PATTERN, ""), "modes it can restart the device into")),
                opt("departures", arr(departure, "")),
            ]),
        )
        .one_of(&["verified", "untried"]),
    );
    let transition = table(
        Table::new(
            "how the device gets from one mode to another (FR-013)",
            tried(vec![
                f("id", sp(ID_PATTERN, "")),
                f("from", sp(ID_PATTERN, "a mode")),
                f("to", sp(ID_PATTERN, "a mode")),
                f(
                    "by",
                    se(&["installer", "hand"], "the installer does it, or the person by hand (texts.transitions.<id>)"),
                ),
                opt("needs", arr(se(&ids(vocab::NEEDS), ""), "what it needs")),
            ]),
        )
        .one_of(&["verified", "untried"]),
    );
    let select = table(
        Table::new(
            "which partitions the rule selects",
            vec![
                opt("name", sp("^[A-Za-z0-9_.-]+$", "")),
                opt("names", arr1(sp("^[A-Za-z0-9_.-]+$", ""), "")),
                opt("pattern", sp("^[A-Za-z0-9_.*-]+$", "`*` stands for any run of characters")),
                opt(
                    "type",
                    sp(
                        "^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$",
                        "a GPT type GUID",
                    ),
                ),
                opt("role", se(&ids(vocab::ROLES), "")),
            ],
        )
        .one_of(&["name", "names", "pattern", "type", "role"]),
    );
    let rule = table(Table::new(
        "a partition rule (FR-014): the first rule that selects a partition decides; unselected means never-write",
        vec![
            f("select", select),
            f("class", se(&ids(vocab::CLASSES), "the most any job may do")),
            opt(
                "destroys",
                boolean("for resize and remove: whether it destroys what is on the partition (FR-015)"),
            ),
            opt("when", when()),
            opt("back_up", back_up()),
        ],
    ));
    let boot_setting = table(Table::new(
        "a boot setting (FR-016)",
        vec![
            f("id", sp(ID_PATTERN, "")),
            f("kind", se(&kinds_ids(vocab::BOOT_SETTING_KINDS), "")),
            f("back_up", back_up()),
        ],
    ));
    let unit_value = table(Table::new(
        "a value unique to the unit that GlassineOS can't read itself (FR-017)",
        vec![
            f("id", sp(ID_PATTERN, "")),
            f("what", s("what it is")),
            f("mode", sp(ID_PATTERN, "the mode it is read in")),
            f("means", s("how it is read")),
            f("goes_to", s("where in the installed system it goes")),
        ],
    ));
    let device_file = table(Table::new(
        "a vendor file the installation reads from the device (FR-018a)",
        vec![
            f("id", sp(ID_PATTERN, "")),
            f("what", s("what it is")),
            f("where", s("where on the device it is read")),
            f("used_by", s("what uses it")),
        ],
    ));
    let artifact = table(Table::new(
        "something the installation writes that doesn't come from the device (FR-019)",
        vec![
            f("role", se(&ids(vocab::ARTIFACT_ROLES), "")),
            f(
                "size",
                Shape::IntOrStr {
                    pattern: "^unknown$",
                    doc: "bytes, or unknown until the release supplies it".into(),
                },
            ),
        ],
    ));
    let capability = table(
        Table::new(
            "a capability of the installed device (FR-020)",
            tried(vec![
                f("id", se(&cap_ids, "")),
                f("status", se(&ids(vocab::CAPABILITY_STATUS), "")),
                opt("with", s("for needs: what it needs")),
                opt("when", when()),
            ]),
        )
        .one_of(&["verified", "untried"]),
    );
    let choice = table(Table::new(
        "something the person decides in the plan (FR-021)",
        vec![
            f("id", sp(ID_PATTERN, "")),
            f("source", se(&ids(vocab::CHOICE_SOURCES), "where the options come from")),
            opt(
                "for_each",
                se(&ids(vocab::CHOICE_TEMPLATES), "a template repeated for each item inspecting finds"),
            ),
            opt(
                "options",
                arr(
                    table(Table::new(
                        "an outcome the person can choose",
                        vec![
                            f("id", sp(ID_PATTERN, "")),
                            opt("replaces_system", boolean("choosing it replaces an operating system")),
                        ],
                    )),
                    "for source = fixed",
                ),
            ),
            f("default", sp(ID_PATTERN, "an option, or for disks: first")),
        ],
    ));
    let job = table(Table::new(
        "a job and the states it is offered in (FR-024)",
        vec![
            f("id", se(&ids(vocab::JOBS), "")),
            f("states", arr1(sp(ID_PATTERN, ""), "the states it is offered in")),
        ],
    ));
    let step = table(
        tagged(
            Table::new(
                "a step of a job (FR-022, FR-023)",
                tried(vec![
                    f("id", sp(ID_PATTERN, "stable id")),
                    f("job", se(&ids(vocab::JOBS), "")),
                    f("kind", se(&kinds_ids(vocab::STEP_KINDS), "a step kind of the description's backend")),
                    f("mode", sp(ID_PATTERN, "the mode it needs")),
                    opt("after", arr(sp(ID_PATTERN, ""), "the steps it comes after")),
                    opt("when", arr(when(), "the choices it depends on (all must hold)")),
                    opt("for_each", se(&ids(vocab::CHOICE_TEMPLATES), "repeated for each item of a template choice")),
                    opt("reads", part_list("partitions it reads")),
                    opt("backs_up", part_list("partitions it backs up")),
                    opt("writes", part_list("partitions it writes")),
                    opt("creates", part_list("partitions it creates")),
                    opt("resizes", part_list("partitions it resizes")),
                    opt("removes", part_list("partitions it removes")),
                    opt("boot_settings", arr(sp(ID_PATTERN, ""), "boot settings it changes (or backs up, for back-up)")),
                    f("done", arr1(condition("a condition that tells the step is done", true, false), "")),
                    f(
                        "escape",
                        table(Table::new(
                            "how the step is escaped",
                            vec![
                                f("failure", escape_case("a failure during it")),
                                f("interruption", escape_case("an interruption right after it")),
                            ],
                        )),
                    ),
                    f("point_of_no_return", boolean("")),
                    f("safe_stop", boolean("a cancel may stop right after it")),
                ]),
            ),
            "kind",
            vocab::STEP_KINDS,
        )
        .one_of(&["verified", "untried"]),
    );
    let texts = table(Table::new(
        "what the installer shows for this device (FR-026): plain text, no markup, English",
        vec![
            f("name", s("the device's name as people see it")),
            opt("modes", text_map("how to reach each mode by hand, by mode id", ID_PATTERN)),
            opt("transitions", text_map("the guide for each by-hand transition, by transition id", ID_PATTERN)),
            opt("choices", text_map("what each choice means here, by choice id", ID_PATTERN)),
            opt(
                "options",
                text_map("what each option means, by choice-id.option-id", "^[a-z0-9-]+\\.[a-z0-9-]+$"),
            ),
            opt("states", text_map("each state in words, by state id", ID_PATTERN)),
            opt("steps", text_map("each step in words, by step id", ID_PATTERN)),
        ],
    ));

    Table::new(
        "A GlassineOS device description, format 1",
        vec![
            f(
                "format",
                Shape::Int {
                    min: Some(1),
                    doc: "the format; this is 1".into(),
                },
            ),
            f("id", sp(ID_PATTERN, "lowercase, - between words, unique among descriptions")),
            f("name", s("shown to people")),
            f("backend", se(&ids(vocab::BACKENDS), "the platform family")),
            opt(
                "generic",
                boolean("matches on its platform alone and is refined by any description that matches too"),
            ),
            opt("refines", sp(ID_PATTERN, "the more general description this one refines")),
            opt(
                "device_data",
                arr(sp("^[A-Za-z0-9_./-]+$", ""), "the device-data files the installed system needs (boot 006)"),
            ),
            f("identity", arr1(identity, "the signals that recognise the device")),
            f("states", arr1(state, "the install states, the device as shipped first")),
            f("modes", arr1(mode, "")),
            opt("transitions", arr(transition, "")),
            f("partitions", arr1(rule, "")),
            opt("boot_settings", arr(boot_setting, "")),
            opt("unit_values", arr(unit_value, "")),
            opt("device_files", arr(device_file, "")),
            opt("artifacts", arr(artifact, "")),
            opt("capabilities", arr(capability, "")),
            opt("choices", arr(choice, "")),
            f("jobs", arr1(job, "")),
            f("steps", arr1(step, "")),
            f("texts", texts),
        ],
    )
}

/// The JSON Schema of format 1, generated from `shape()`.
pub fn json_schema() -> serde_json::Value {
    let mut v = shape().json_schema();
    if let serde_json::Value::Object(m) = &mut v {
        m.insert("$schema".into(), "https://json-schema.org/draft/2020-12/schema".into());
        m.insert(
            "$id".into(),
            "https://github.com/GlassineOS/installer/schema/device-description-1.schema.json".into(),
        );
        m.insert("title".into(), "GlassineOS device description, format 1".into());
        m.insert("x-vocabularies".into(), vocabularies_json());
        if let Some(serde_json::Value::Object(props)) = m.get_mut("properties") {
            props.insert("format".into(), serde_json::json!({ "const": 1, "description": "the format; this is 1" }));
        }
    }
    v
}

/// The vocabularies with their meanings and parameters, in the schema too.
pub fn vocabularies_json() -> serde_json::Value {
    use serde_json::json;
    let entries = |l: &[vocab::Entry]| l.iter().map(|e| json!({ "id": e.id, "doc": e.doc })).collect::<Vec<_>>();
    let kinds = |l: &[vocab::Kind]| {
        l.iter()
            .map(|k| {
                json!({ "id": k.id, "doc": k.doc, "backends": k.backends, "modes": k.modes,
                    "params": k.params.iter().map(|p| json!({ "name": p.name, "type": format!("{:?}", p.ty), "required": p.required, "doc": p.doc })).collect::<Vec<_>>() })
            })
            .collect::<Vec<_>>()
    };
    json!({
        "fact_keys": vocab::FACT_KEYS.iter().map(|k| json!({ "key": k.pattern, "type": format!("{:?}", k.ty), "kind": k.kind.name(), "modes": k.modes, "unit_unique": k.unit_unique, "doc": k.doc })).collect::<Vec<_>>(),
        "mode_kinds": vocab::MODE_KINDS.iter().map(|m| json!({ "id": m.id, "doc": m.doc, "backends": m.backends, "operations": m.operations })).collect::<Vec<_>>(),
        "departures": kinds(vocab::DEPARTURES),
        "step_kinds": kinds(vocab::STEP_KINDS),
        "boot_setting_kinds": kinds(vocab::BOOT_SETTING_KINDS),
        "backup_means": kinds(vocab::BACKUP_MEANS),
        "capabilities": vocab::CAPABILITIES.iter().map(|c| json!({ "id": c.id, "doc": c.doc, "needed_to_boot_safely": c.safe_boot })).collect::<Vec<_>>(),
        "partition_classes": entries(vocab::CLASSES),
        "partition_actions": entries(vocab::ACTIONS),
        "partition_roles": entries(vocab::ROLES),
        "condition_tests": entries(vocab::CONDITION_TESTS),
        "needs": entries(vocab::NEEDS),
        "escape_leaves": entries(vocab::ESCAPE_LEAVES),
        "jobs": entries(vocab::JOBS),
        "backends": entries(vocab::BACKENDS),
    })
}
