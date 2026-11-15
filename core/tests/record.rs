// SPDX-License-Identifier: GPL-2.0-or-later
//! The install record: round trip, the journal's rules, resume after an
//! interruption at every step, and saves cut at every point.

mod common;
use common::root;
use glassine_installer::artifacts::Artifact;
use glassine_installer::lock::sha256_hex;
use glassine_installer::record::{Backup, Confirmation, Outcome, RECORD_FORMAT, Record, RecordStore, Result_, Resume, StepState, load, save};
use std::collections::BTreeMap;

const PLAN: &str = r#"{"plan_format": 1, "status": "ready",
  "steps": [{"id": "back-up"}, {"id": "partition-table"}, {"id": "write-kernel"}]}"#;
const STEPS: [&str; 3] = ["back-up", "partition-table", "write-kernel"];

fn record() -> Record {
    Record::new(
        "0.1.0",
        "onyx-noteair",
        b"format = 3\n",
        PLAN,
        Confirmation {
            text: "Install".into(),
            at: Some("2026-11-15T10:00:00Z".into()),
        },
        vec![Artifact {
            role: "kernel".into(),
            file: "kernel.img".into(),
            sha256: "ab".repeat(32),
            size: 4096,
            kind: Some("raw-image".into()),
        }],
        Some(Backup {
            location: "backups/noteair-2026-11-15".into(),
            manifest_sha256: None,
        }),
    )
    .unwrap()
}

fn done(evidence: &[(&str, &str)]) -> Outcome {
    Outcome {
        result: Result_::Done,
        evidence: evidence.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        message: None,
        at: None,
    }
}

#[test]
fn a_record_round_trips_and_keeps_the_plan_verbatim() {
    let mut r = record();
    assert_eq!(r.record_format, RECORD_FORMAT);
    assert_eq!(r.plan, PLAN);
    assert_eq!(r.plan_sha256, sha256_hex(PLAN.as_bytes()));
    assert_eq!(r.description.sha256, sha256_hex(b"format = 3\n"));
    r.begin("back-up", "read every partition", None).unwrap();
    r.finish("back-up", done(&[("manifest_sha256", "cd")])).unwrap();
    let back = Record::parse(&r.to_json()).unwrap();
    assert_eq!(back, r);
    assert_eq!(back.done().unwrap(), vec!["back-up"]);
}

#[test]
fn a_real_plan_is_taken_as_it_is() {
    let p = std::fs::read_to_string(root().join("core/tests/golden/noteair-cable-yes.json")).unwrap();
    let r = Record::new("0.1.0", "onyx-noteair", b"", &p, Confirmation { text: "x".into(), at: None }, vec![], None).unwrap();
    assert!(!r.plan_steps().unwrap().is_empty());
    assert_eq!(Record::parse(&r.to_json()).unwrap().plan, p);
}

#[test]
fn a_changed_plan_a_newer_format_or_a_broken_journal_is_refused() {
    let r = record();
    let t = r.to_json().replace("\\\"ready\\\"", "\\\"refused\\\"");
    assert_ne!(t, r.to_json());
    assert!(Record::parse(&t).unwrap_err().contains("doesn't match its hash"));
    let t = r.to_json().replace("\"record_format\": 1", "\"record_format\": 2");
    assert!(Record::parse(&t).unwrap_err().contains("record format 2"));
    let mut r = record();
    r.begin("back-up", "", None).unwrap();
    r.journal.push(r.journal[0].clone());
    assert!(Record::parse(&r.to_json()).unwrap_err().contains("open entry for back-up before others"));
    assert!(Record::new("0", "x", b"", "{}", Confirmation { text: "".into(), at: None }, vec![], None).is_err());
}

#[test]
fn the_journal_takes_intent_before_and_outcome_after() {
    let mut r = record();
    assert!(r.finish("back-up", done(&[])).unwrap_err().contains("no open entry"));
    assert!(r.begin("format-everything", "", None).unwrap_err().contains("no step format-everything"));
    r.begin("back-up", "", None).unwrap();
    assert_eq!(r.state("back-up"), StepState::Open);
    assert!(r.begin("partition-table", "", None).unwrap_err().contains("back-up is still open"));
    assert!(r.finish("partition-table", done(&[])).is_err());
    r.finish("back-up", done(&[])).unwrap();
    assert!(r.begin("back-up", "", None).unwrap_err().contains("already done"));
}

#[test]
fn resume_after_an_interruption_in_each_step() {
    for (i, cut) in STEPS.iter().enumerate() {
        let mut r = record();
        for s in &STEPS[..i] {
            r.begin(s, "", None).unwrap();
            r.finish(s, done(&[])).unwrap();
        }
        r.begin(cut, "", None).unwrap();
        // The process dies here; what was saved is read back.
        let mut r = Record::parse(&r.to_json()).unwrap();
        assert_eq!(r.resume().unwrap(), Resume::Interrupted(cut.to_string()));
        assert_eq!(r.done().unwrap(), STEPS[..i].to_vec());
        // Checked and found incomplete: closed as interrupted, then run again.
        r.finish(
            cut,
            Outcome {
                result: Result_::Interrupted,
                evidence: BTreeMap::new(),
                message: None,
                at: None,
            },
        )
        .unwrap();
        assert_eq!(r.resume().unwrap(), Resume::Next(cut.to_string()));
        r.begin(cut, "again", None).unwrap();
        r.finish(cut, done(&[])).unwrap();
    }
}

#[test]
fn resume_after_a_failure_and_at_the_end() {
    let mut r = record();
    r.begin("back-up", "", None).unwrap();
    r.finish(
        "back-up",
        Outcome {
            result: Result_::Failed,
            evidence: BTreeMap::new(),
            message: None,
            at: None,
        },
    )
    .unwrap();
    assert_eq!(r.resume().unwrap(), Resume::Failed("back-up".into()));
    for s in STEPS {
        r.begin(s, "", None).unwrap();
        r.finish(s, done(&[])).unwrap();
    }
    assert_eq!(r.resume().unwrap(), Resume::Complete);
}

/// A store that can be cut at any point of a save.
#[derive(Default)]
struct Store {
    record: Option<Vec<u8>>,
    temp: Option<Vec<u8>>,
    /// Cut the next temp write after this many bytes.
    cut_write: Option<usize>,
    fail_commit: bool,
}

impl RecordStore for Store {
    fn write_temp(&mut self, bytes: &[u8]) -> Result<(), String> {
        if let Some(n) = self.cut_write.take() {
            self.temp = Some(bytes[..n].to_vec());
            return Err("cut".into());
        }
        self.temp = Some(bytes.to_vec());
        Ok(())
    }
    fn commit(&mut self) -> Result<(), String> {
        if self.fail_commit {
            return Err("cut before the rename".into());
        }
        self.record = Some(self.temp.take().ok_or("no temp")?);
        Ok(())
    }
    fn read(&mut self) -> Result<Option<Vec<u8>>, String> {
        Ok(self.record.clone())
    }
}

#[test]
fn a_cut_save_leaves_the_previous_record() {
    let mut st = Store::default();
    assert_eq!(load(&mut st).unwrap(), None);
    let mut r = record();
    save(&mut st, &r).unwrap();
    let before = load(&mut st).unwrap().unwrap();
    assert_eq!(before, r);
    r.begin("back-up", "", None).unwrap();
    let full = r.to_json().len();
    for n in [0, 1, full / 2, full - 1] {
        st.cut_write = Some(n);
        assert!(save(&mut st, &r).is_err());
        assert_eq!(load(&mut st).unwrap().unwrap(), before, "cut after {n} bytes");
    }
    st.fail_commit = true;
    assert!(save(&mut st, &r).is_err());
    assert_eq!(load(&mut st).unwrap().unwrap(), before);
    st.fail_commit = false;
    save(&mut st, &r).unwrap();
    assert_eq!(load(&mut st).unwrap().unwrap().resume().unwrap(), Resume::Interrupted("back-up".into()));
}
