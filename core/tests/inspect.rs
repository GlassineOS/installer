// SPDX-License-Identifier: GPL-2.0-or-later
//! Live inspect against simulated fastboot devices (002 FR-011, FR-012; 001
//! FR-001, Scenario 2): the framing chosen from the opening, the facts, no
//! serial, the warm-up's refusal, timeouts, and recognition and plan from the
//! live snapshot equal to the fixture-based goldens.

mod common;
use common::*;
use glassine_installer::fastboot::{FbError, Framing, Transport, readable_variable};
use glassine_installer::format1::Description;
use glassine_installer::inspect::{Host, InspectError, Inspected, UsbIds, inspect, reads};
use glassine_installer::plan::{Input, plan, render};
use glassine_installer::recognize::recognize;
use std::collections::{BTreeMap, VecDeque};

const LEN: usize = 65535;

/// A fastboot device. With `bug`, its USB driver counts each OUT transfer as
/// the length of the previous one, as the Note Air's does (GlassineOS/abl#1):
/// the first transfer of a boot is counted right, the second as 6 bytes, every
/// later one as the length of the one before it; its 64-byte command buffer
/// keeps what the last command left in it. Without it, a device as the
/// protocol specifies: a transfer is one command, at most 64 bytes.
struct Sim {
    vars: BTreeMap<String, String>,
    bug: bool,
    n: usize,
    prev: usize,
    cmdbuf: [u8; 64],
    replies: VecDeque<Vec<u8>>,
    /// Every OUT transfer, as sent.
    log: Vec<Vec<u8>>,
    /// Stops answering after this many transfers.
    silent_after: Option<usize>,
    /// Answers every getvar after this many transfers with this.
    lie_after: Option<(usize, &'static str)>,
    rebooted: bool,
}

impl Sim {
    fn new(vars: BTreeMap<String, String>, bug: bool) -> Sim {
        Sim {
            vars,
            bug,
            n: 0,
            prev: 0,
            cmdbuf: [0; 64],
            replies: VecDeque::new(),
            log: vec![],
            silent_after: None,
            lie_after: None,
            rebooted: false,
        }
    }

    /// Answers what a facts fixture holds (a FAIL for its nulls and anything
    /// else), and its serial number when asked.
    fn from_fixture(name: &str, bug: bool) -> Sim {
        let f = facts(name);
        let mut vars: BTreeMap<String, String> = f.fastboot.unwrap().variables.into_iter().filter_map(|(k, v)| v.map(|v| (k, v))).collect();
        vars.insert("serialno".into(), "SECRET-UNIT-1234".into());
        Sim::new(vars, bug)
    }

    /// A device after an earlier session in the same boot: not its first
    /// transfers, and an old command still in the buffer.
    fn warm(mut self, prev: usize, old: &[u8]) -> Sim {
        self.n = 5;
        self.prev = prev;
        self.cmdbuf[..old.len()].copy_from_slice(old);
        self
    }

    fn command(&mut self, d: &[u8]) -> Option<String> {
        if self.bug {
            let count = match self.n {
                0 => d.len(),
                1 => 6,
                _ => self.prev,
            };
            let k = d.len().min(64);
            self.cmdbuf[..k].copy_from_slice(&d[..k]);
            let mut cmd = self.cmdbuf[..count.min(63)].to_vec();
            if let Some(z) = cmd.iter().position(|&b| b == 0) {
                cmd.truncate(z);
            }
            Some(String::from_utf8_lossy(&cmd).into_owned())
        } else if d.len() > 64 {
            None
        } else {
            Some(String::from_utf8_lossy(d).into_owned())
        }
    }
}

impl Transport for Sim {
    async fn send(&mut self, d: &[u8]) -> Result<(), FbError> {
        self.log.push(d.to_vec());
        let cmd = self.command(d);
        self.n += 1;
        self.prev = d.len();
        if self.silent_after.is_some_and(|k| self.log.len() > k) {
            return Ok(());
        }
        let lie = self.lie_after.filter(|(k, _)| self.log.len() > *k).map(|(_, v)| v);
        let r = match cmd.as_deref() {
            None => b"FAILcommand too long".to_vec(),
            Some(c) => match c.strip_prefix("getvar:") {
                Some(_) if lie.is_some() => format!("OKAY{}", lie.unwrap()).into_bytes(),
                Some(v) => match self.vars.get(v) {
                    Some(x) => format!("OKAY{x}").into_bytes(),
                    None => b"FAILunknown variable".to_vec(),
                },
                None if c == "reboot" => {
                    self.rebooted = true;
                    b"OKAY".to_vec()
                }
                None => b"FAILunknown command".to_vec(),
            },
        };
        self.replies.push_back(r);
        Ok(())
    }

    async fn recv(&mut self) -> Result<Vec<u8>, FbError> {
        // A real transport bounds the wait and cancels; the simulation's wait
        // ends at once.
        self.replies.pop_front().ok_or(FbError::Timeout("no answer".into()))
    }
}

fn block<F: Future>(f: F) -> F::Output {
    pollster::block_on(f)
}

fn descriptions() -> Vec<Description> {
    vec![valid(&noteair()), valid(&pc())]
}

fn fastboot_usb() -> UsbIds {
    UsbIds {
        vendor_id: 0x18d1,
        product_id: 0xd00d,
        interfaces: vec![(0xff, 0x42, 0x03)],
    }
}

fn host() -> Host {
    Host {
        platform: "linux".into(),
        connections: vec!["usb-native".into()],
    }
}

fn run_on(ds: &[Description], sim: &mut Sim, reboot: bool) -> Result<Inspected, InspectError> {
    let refs: Vec<&Description> = ds.iter().collect();
    block(inspect(&refs, sim, &fastboot_usb(), &host(), reboot))
}

fn run(sim: &mut Sim) -> Result<Inspected, InspectError> {
    run_on(&descriptions(), sim, false)
}

/// Every transfer is the opening, the warm-up's dummy, one readable getvar
/// (plain or NUL-padded to the one length), or a reboot when one was asked.
fn only_reads(log: &[Vec<u8>], reboot_asked: bool) {
    assert_eq!(log.first().map(Vec::as_slice), Some(&b"getvar:product"[..]), "the opening");
    for t in log {
        if t.len() == LEN && t.iter().all(|&b| b == b'x') {
            continue;
        }
        let end = t.iter().position(|&b| b == 0).unwrap_or(t.len());
        assert!(t[end..].iter().all(|&b| b == 0), "padding is NUL only");
        assert!(t.len() == end || t.len() == LEN, "a padded command is {LEN} bytes");
        let cmd = std::str::from_utf8(&t[..end]).unwrap();
        if cmd == "reboot" {
            assert!(reboot_asked, "reboot sent without being asked");
            continue;
        }
        let v = cmd.strip_prefix("getvar:").unwrap_or_else(|| panic!("not a read: {cmd:?}"));
        assert!(readable_variable(v).is_ok(), "{v:?}");
        assert_ne!(v, "all");
        assert!(!v.contains("serial"), "{v:?}");
    }
}

fn padded(log: &[Vec<u8>]) -> usize {
    log.iter().filter(|t| t.len() == LEN).count()
}

/// The live snapshot holds every variable the fixture does, with its value.
fn covers_fixture(got: &Inspected, fixture: &str) {
    let want = facts(fixture).fastboot.unwrap().variables;
    let live = &got.snapshot.fastboot.as_ref().unwrap().variables;
    for (k, v) in &want {
        assert_eq!(live.get(k), Some(v), "{k}");
    }
    assert_eq!(
        serde_json::to_value(&got.snapshot.usb).unwrap(),
        serde_json::to_value(&facts(fixture).usb).unwrap()
    );
}

fn no_serial(got: &Inspected) {
    let json = serde_json::to_string(&got.snapshot).unwrap();
    assert!(!json.contains("SECRET-UNIT-1234"), "{json}");
    assert!(!json.contains("serial"), "{json}");
}

/// Compares with a golden file without ever writing it: these goldens are the
/// fixture-based ones of tests/plan.rs.
fn same_as_golden(rel: &str, got: &str) {
    let want = std::fs::read_to_string(root().join(rel)).unwrap();
    assert_eq!(want, got, "{rel}: the live snapshot plans differently from the fixture");
}

#[test]
fn the_stock_note_air_gets_equal_length_framing_from_its_first_answer() {
    let mut sim = Sim::from_fixture("noteair-stock-fastboot", true);
    let got = run(&mut sim).unwrap();
    assert_eq!(got.framing, Framing::EqualLength { len: LEN });
    assert_eq!(got.told_by, vec!["onyx-noteair mode fastboot-stock"]);
    covers_fixture(&got, "noteair-stock-fastboot");
    no_serial(&got);
    only_reads(&sim.log, false);
    assert!(!sim.rebooted);
    // The opening, the dummy, then every read padded, the check before and after.
    assert_eq!(padded(&sim.log), sim.log.len() - 1);
    assert_eq!(sim.log.len(), 1 + 1 + 1 + reads(&descriptions().iter().collect::<Vec<_>>()).len() + 1);
}

#[test]
fn the_glassine_boot_loader_gets_equal_length_framing_too() {
    let mut sim = Sim::from_fixture("noteair-prepared-fastboot", true);
    let got = run(&mut sim).unwrap();
    assert_eq!(got.framing, Framing::EqualLength { len: LEN });
    assert_eq!(got.told_by, vec!["onyx-noteair mode fastboot"]);
    covers_fixture(&got, "noteair-prepared-fastboot");
    no_serial(&got);
    only_reads(&sim.log, false);
}

#[test]
fn plain_framing_would_have_failed_on_the_note_air() {
    // What the probe protects against: the Note Air's second plain command is
    // misread (abl#1). A plain-framing device that no description knows reads
    // normally (next test); the same reads on the bug lose the answers.
    let mut sim = Sim::from_fixture("noteair-stock-fastboot", true);
    sim.vars.insert("product".into(), "some-board".into());
    let got = run(&mut sim);
    // `product` read a second time plain is misread as `getvar` and FAILs:
    // refused, no snapshot.
    assert!(matches!(got, Err(InspectError::NotFresh(_))), "{got:?}");
}

#[test]
fn a_device_no_description_knows_stays_on_plain_framing() {
    let mut sim = Sim::from_fixture("unknown-fastboot", false);
    let got = run(&mut sim).unwrap();
    assert_eq!(got.framing, Framing::Plain);
    assert!(got.told_by.is_empty());
    assert_eq!(padded(&sim.log), 0);
    covers_fixture(&got, "unknown-fastboot");
    no_serial(&got);
    only_reads(&sim.log, false);
    let ds = descriptions();
    let refs: Vec<&Description> = ds.iter().collect();
    let live = serde_json::to_value(recognize(&refs, &got.snapshot).outcome).unwrap();
    let fixture = serde_json::to_value(recognize(&refs, &facts("unknown-fastboot")).outcome).unwrap();
    assert_eq!(live, fixture);
}

#[test]
fn a_standard_device_that_answers_like_the_stock_note_air_is_refused_not_misread() {
    // A conforming boot loader answering product=sdm660 gets the warm-up; it
    // answers the padded check with FAIL (too long), so inspect refuses.
    let mut sim = Sim::from_fixture("noteair-stock-fastboot", false);
    let got = run(&mut sim);
    assert!(matches!(got, Err(InspectError::NotFresh(_))), "{got:?}");
    assert_eq!(sim.log.len(), 3, "nothing after the failed check");
    only_reads(&sim.log, false);
}

#[test]
fn a_used_note_air_is_not_fresh() {
    // After an earlier padded download the buffer holds a longer command, so
    // the opening is misread and FAILs; these USB ids belong to a mode that
    // needs a fresh start, so it is "not fresh", not an unknown device.
    for old in [&b"download:000ffff0"[..], b"getvar:partition-size:userdata"] {
        let mut sim = Sim::from_fixture("noteair-prepared-fastboot", true).warm(LEN, old);
        let got = run(&mut sim);
        assert!(matches!(&got, Err(InspectError::NotFresh(w)) if w.contains("opening")), "{got:?}");
        assert_eq!(sim.log.len(), 1, "nothing after the opening");
    }
}

#[test]
fn a_used_note_air_whose_buffer_happens_to_agree_reads_right() {
    // The last command of an inspect is the padded product check, so a second
    // inspect in the same boot reads the opening right; the checks pass and the
    // facts are correct, never misread.
    let mut sim = Sim::from_fixture("noteair-prepared-fastboot", true).warm(LEN, b"getvar:product");
    let got = run(&mut sim).unwrap();
    covers_fixture(&got, "noteair-prepared-fastboot");
}

#[test]
fn answers_that_slip_during_the_session_give_no_snapshot() {
    let mut sim = Sim::from_fixture("noteair-stock-fastboot", true);
    sim.lie_after = Some((20, "garbage"));
    let got = run(&mut sim);
    assert!(matches!(&got, Err(InspectError::NotFresh(w)) if w.contains("after the reads")), "{got:?}");
}

#[test]
fn a_device_that_answers_nothing_times_out() {
    for k in [0, 1, 2, 10] {
        let mut sim = Sim::from_fixture("noteair-stock-fastboot", true);
        sim.silent_after = Some(k);
        let got = run(&mut sim);
        assert!(matches!(got, Err(InspectError::Timeout(_))), "after {k}: {got:?}");
        assert_eq!(sim.log.len(), k + 1, "nothing sent after the wait ran out");
    }
}

#[test]
fn reboot_only_when_asked_and_last() {
    let mut sim = Sim::from_fixture("noteair-prepared-fastboot", true);
    let got = run_on(&descriptions(), &mut sim, true).unwrap();
    assert!(got.rebooted && sim.rebooted);
    only_reads(&sim.log, true);
    let last = sim.log.last().unwrap();
    assert_eq!(&last[..7], b"reboot\0");
    // A reboot that isn't answered leaves the facts standing.
    let mut sim = Sim::from_fixture("noteair-prepared-fastboot", true);
    sim.silent_after = Some(1 + 1 + 1 + reads(&descriptions().iter().collect::<Vec<_>>()).len() + 1);
    let got = run_on(&descriptions(), &mut sim, true).unwrap();
    assert!(!got.rebooted && got.reboot_error.is_some());
    covers_fixture(&got, "noteair-prepared-fastboot");
}

#[test]
fn descriptions_that_tell_two_framings_for_one_answer_are_an_error() {
    let t = mutate(
        &noteair(),
        "params = { length = 65535, warm-up = \"short-command-then-dummy\" }\ntold_by = { key = \"fastboot.var.product\", equals = \"NoteAir\" }",
        "params = { length = 4095, warm-up = \"short-command-then-dummy\" }\ntold_by = { key = \"fastboot.var.product\", equals = \"sdm660\" }",
    );
    let mut sim = Sim::from_fixture("noteair-stock-fastboot", true);
    let got = run_on(&[valid(&t)], &mut sim, false);
    assert!(matches!(&got, Err(InspectError::Descriptions(_))), "{got:?}");
    assert_eq!(sim.log.len(), 1, "nothing after the opening");
}

#[test]
fn reads_follow_the_descriptions() {
    let ds = descriptions();
    let refs: Vec<&Description> = ds.iter().collect();
    let r = reads(&refs);
    for want in [
        "variant",
        "hw-revision",
        "current-slot",
        "slot-count",
        "partition-size:onyxconfig",
        "partition-type:onyxconfig",
        "partition-size:boot_b",
        "partition-size:abl",
        "partition-size:userdata",
    ] {
        assert!(r.iter().any(|x| x == want), "{want}");
    }
    // The PC's partitions are not a boot loader's to answer.
    assert!(!r.iter().any(|x| x.contains("glassine-system")));
    assert!(!r.iter().any(|x| x == "product" || x == "all" || x.contains("serial")));
}

#[test]
fn recognition_and_plan_from_the_live_snapshot_equal_the_goldens() {
    let ds = descriptions();
    let refs: Vec<&Description> = ds.iter().collect();
    let mut sim = Sim::from_fixture("noteair-stock-fastboot", true);
    let live = run(&mut sim).unwrap().snapshot;
    // The snapshot survives the trip through JSON a page or --out makes.
    let live = glassine_installer::facts::Snapshot::parse(&serde_json::to_string(&live).unwrap()).unwrap();
    let rec = recognize(&refs, &live);
    let fixture_rec = recognize(&refs, &facts("noteair-stock-fastboot"));
    assert_eq!(serde_json::to_value(&rec.outcome).unwrap(), serde_json::to_value(&fixture_rec.outcome).unwrap());
    let input = |choices: &[(&str, &str)], hardware: &[(&str, bool)], developer: bool| Input {
        choices: choices.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        hardware: hardware.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        developer,
        ..Default::default()
    };
    for (name, inp) in [
        ("noteair-keep-stock", Input::default()),
        ("noteair-no-stock-developer", input(&[("keep-stock", "no")], &[("emergency-cable", true)], true)),
        ("noteair-cable-yes", input(&[], &[("emergency-cable", true)], false)),
        ("noteair-cable-no", input(&[], &[("emergency-cable", false)], true)),
    ] {
        let p = plan(&refs, &live, &rec, &inp);
        same_as_golden(&format!("core/tests/golden/{name}.json"), &(serde_json::to_string_pretty(&p).unwrap() + "\n"));
        same_as_golden(&format!("core/tests/golden/{name}.txt"), &render(&p, rec.matched(&refs)));
    }
}
