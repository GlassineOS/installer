// SPDX-License-Identifier: GPL-2.0-or-later
//! Recognition: one facts snapshot against every description (FR-007,
//! FR-007a), then the matched description's install state (FR-009).

use crate::facts::{Obs, Snapshot, Truth, test};
use crate::format1::{Condition, Description};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize)]
pub struct SignalResult {
    pub key: String,
    pub test: String,
    pub required: bool,
    pub result: Truth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Matched,
    /// Some of its own required signals hold and another contradicts.
    Conflict,
    /// Some required signals hold, none contradicts, some can't be seen in this mode.
    Partial,
    NoMatch,
}

#[derive(Debug, Clone, Serialize)]
pub struct DescriptionResult {
    pub id: String,
    pub verdict: Verdict,
    pub signals: Vec<SignalResult>,
    /// Modes of this description that would decide its undetermined signals.
    pub needs_modes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum StateResult {
    Decided {
        state: String,
    },
    /// Not decided in this mode; these modes would decide it.
    Undetermined {
        holding: Vec<String>,
        undetermined: Vec<String>,
        needs_modes: Vec<String>,
    },
    /// More than one state holds once decided.
    Conflict {
        states: Vec<String>,
    },
    /// No state's conditions hold.
    None,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum Outcome {
    Match {
        id: String,
        state: StateResult,
    },
    /// Treated as unsupported; the report names the conflicting signals.
    Conflict {
        ids: Vec<String>,
    },
    /// Two matches neither of which refines the other: unsupported.
    Ambiguous {
        ids: Vec<String>,
    },
    /// It may be one of these; another mode would tell.
    Undetermined {
        candidates: Vec<String>,
        needs_modes: Vec<String>,
    },
    None,
}

#[derive(Debug, Clone, Serialize)]
pub struct Recognition {
    pub mode: String,
    pub outcome: Outcome,
    pub descriptions: Vec<DescriptionResult>,
}

impl Recognition {
    pub fn matched<'a>(&self, ds: &[&'a Description]) -> Option<&'a Description> {
        match &self.outcome {
            Outcome::Match { id, .. } => ds.iter().find(|d| d.id == *id).copied(),
            _ => None,
        }
    }
}

fn ancestors<'a>(ds: &[&'a Description], d: &Description) -> Vec<&'a Description> {
    let mut out: Vec<&Description> = Vec::new();
    let mut cur = d.refines.as_deref();
    while let Some(id) = cur {
        match ds.iter().find(|x| x.id == id) {
            Some(p) if !out.iter().any(|x| x.id == p.id) => {
                out.push(p);
                cur = p.refines.as_deref();
            }
            _ => break,
        }
    }
    out
}

fn same(a: &Condition, b: &Condition) -> bool {
    a.describe() == b.describe()
}

fn evaluate(ds: &[&Description], d: &Description, facts: &Snapshot) -> DescriptionResult {
    let anc = ancestors(ds, d);
    let inherited = |c: &Condition| anc.iter().any(|p| p.identity.iter().any(|x| x.required == Some(true) && same(x, c)));
    let mut signals = Vec::new();
    let (mut own_true, mut any_false, mut any_unknown, mut all_true) = (false, false, false, true);
    let mut unknown_keys = Vec::new();
    for c in &d.identity {
        let r = test(c, &facts.observe(&c.key, &facts.mode));
        let req = c.required == Some(true);
        signals.push(SignalResult {
            key: c.key.clone(),
            test: c.describe(),
            required: req,
            result: r,
        });
        if !req {
            continue;
        }
        match r {
            Truth::True if !inherited(c) => own_true = true,
            Truth::True => {}
            Truth::False => any_false = true,
            Truth::Undetermined => {
                any_unknown = true;
                unknown_keys.push(c.key.clone());
            }
        }
        all_true &= r == Truth::True;
    }
    let verdict = if all_true {
        Verdict::Matched
    } else if any_false && own_true {
        Verdict::Conflict
    } else if any_false {
        Verdict::NoMatch
    } else if any_unknown && signals.iter().any(|s| s.required && s.result == Truth::True) {
        Verdict::Partial
    } else {
        Verdict::NoMatch
    };
    let mut needs = BTreeSet::new();
    for k in &unknown_keys {
        if let Some((fk, _)) = crate::vocab::fact_key(k) {
            for m in &d.modes {
                if fk.modes.contains(&m.kind.as_str()) {
                    needs.insert(m.id.clone());
                }
            }
        }
    }
    DescriptionResult {
        id: d.id.clone(),
        verdict,
        signals,
        needs_modes: needs.into_iter().collect(),
    }
}

/// The truth of each state in the snapshot's mode: only conditions observable
/// in that mode count; a state with none there is undetermined.
pub fn states(d: &Description, facts: &Snapshot) -> StateResult {
    let mode_kind = |m: &str| -> String {
        if m == crate::vocab::INSTALLER_MODE {
            m.to_string()
        } else {
            d.mode(m).map(|x| x.kind.clone()).unwrap_or_default()
        }
    };
    let here = |c: &Condition| {
        let k = mode_kind(c.mode.as_deref().unwrap_or_default());
        k == facts.mode
    };
    let (mut holding, mut undetermined, mut needs) = (Vec::new(), Vec::new(), BTreeSet::new());
    let mut elsewhere = Vec::new();
    for s in &d.states {
        let in_mode: Vec<&Condition> = s.conditions.iter().filter(|c| here(c)).collect();
        let others: BTreeSet<String> = s.conditions.iter().filter(|c| !here(c)).filter_map(|c| c.mode.clone()).collect();
        if in_mode.is_empty() {
            undetermined.push(s.id.clone());
            needs.extend(others);
            continue;
        }
        let truths: Vec<Truth> = in_mode
            .iter()
            .map(|c| test(c, &facts.observe(&c.key, &mode_kind(c.mode.as_deref().unwrap_or_default()))))
            .collect();
        if truths.contains(&Truth::False) {
            continue;
        }
        if truths.contains(&Truth::Undetermined) {
            undetermined.push(s.id.clone());
            needs.extend(others);
            continue;
        }
        holding.push(s.id.clone());
        elsewhere.push(others);
    }
    if holding.len() >= 2 {
        if elsewhere.iter().all(|o| !o.is_empty()) {
            let mut n = needs;
            n.extend(elsewhere.into_iter().flatten());
            return StateResult::Undetermined {
                holding,
                undetermined,
                needs_modes: n.into_iter().collect(),
            };
        }
        return StateResult::Conflict { states: holding };
    }
    if !undetermined.is_empty() {
        return StateResult::Undetermined {
            holding,
            undetermined,
            needs_modes: needs.into_iter().collect(),
        };
    }
    match holding.pop() {
        Some(state) => StateResult::Decided { state },
        None => StateResult::None,
    }
}

/// Recognises the machine of one snapshot among all descriptions.
pub fn recognize(ds: &[&Description], facts: &Snapshot) -> Recognition {
    let results: Vec<DescriptionResult> = ds.iter().map(|d| evaluate(ds, d, facts)).collect();
    let verdict = |id: &str| results.iter().find(|r| r.id == id).map(|r| r.verdict);
    // A refinement only matches where what it refines matches too.
    let matched: Vec<&Description> = ds
        .iter()
        .copied()
        .filter(|d| verdict(&d.id) == Some(Verdict::Matched) && ancestors(ds, d).iter().all(|p| verdict(&p.id) == Some(Verdict::Matched)))
        .collect();
    let refined_by_other = |d: &Description| matched.iter().any(|m| m.id != d.id && ancestors(ds, m).iter().any(|a| a.id == d.id));
    let winners: Vec<&Description> = matched.iter().copied().filter(|d| !refined_by_other(d)).collect();
    // A refinement that half-matches a matched description is just not that
    // refinement; any other conflict makes the machine unsupported.
    let blocking: Vec<String> = ds
        .iter()
        .filter(|d| {
            let v = verdict(&d.id);
            let broken_refinement = v == Some(Verdict::Matched) && !matched.iter().any(|m| m.id == d.id);
            (v == Some(Verdict::Conflict) || broken_refinement) && !ancestors(ds, d).iter().any(|a| matched.iter().any(|m| m.id == a.id))
        })
        .map(|d| d.id.clone())
        .collect();
    let outcome = if !blocking.is_empty() {
        Outcome::Conflict { ids: blocking }
    } else if winners.len() >= 2 {
        Outcome::Ambiguous {
            ids: winners.iter().map(|d| d.id.clone()).collect(),
        }
    } else if let Some(w) = winners.first() {
        Outcome::Match {
            id: w.id.clone(),
            state: states(w, facts),
        }
    } else {
        let partial: Vec<&DescriptionResult> = results.iter().filter(|r| r.verdict == Verdict::Partial).collect();
        if partial.is_empty() {
            Outcome::None
        } else {
            let needs: BTreeSet<String> = partial.iter().flat_map(|r| r.needs_modes.iter().cloned()).collect();
            Outcome::Undetermined {
                candidates: partial.iter().map(|r| r.id.clone()).collect(),
                needs_modes: needs.into_iter().collect(),
            }
        }
    };
    Recognition {
        mode: facts.mode.clone(),
        outcome,
        descriptions: results,
    }
}

/// For callers that need the observation of a description's fact directly.
pub fn observe(facts: &Snapshot, key: &str) -> Obs {
    facts.observe(key, &facts.mode)
}
