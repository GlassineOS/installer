// SPDX-License-Identifier: GPL-2.0-or-later
// What the screens show, worked out from the core's records (a check report, a plan) and the
// device description's texts. Pure functions of a session: { report, plan, desc, kind, ... }.

import {
  LEAVES, BOOT_SETTING, DEVICE_DATA, SYSTEM_KIND, MODE, cap, list, human,
} from './words.js';

export function machineOf(report, desc) {
  const kind = desc ? (desc.backend === 'uefi-pc' ? 'pc' : 'device') : report && report.mode === 'local' ? 'pc' : 'device';
  const name = (desc && (desc.texts.name || desc.name)) || (report && report.recognition && report.recognition.name) || null;
  const the = kind === 'pc' ? 'this PC' : name ? 'the ' + name : 'the device';
  const chip = kind === 'pc' ? 'This PC' : (name || 'Unrecognised device') + ' · USB';
  return { kind, name, the, chip };
}

// "Replace the boot loader with GlassineOS's. From here ..." -> ["Replace ...'s.", "From here ..."]
export function splitSentence(text) {
  const m = /^(.+?[.!?])\s+(.*)$/s.exec(text || '');
  return m ? [m[1], m[2]] : [text || '', ''];
}

export function systemView(label) {
  const m = /^system (\d+) \(([a-z]+)\)$/.exec(label || '');
  if (m) return { display: SYSTEM_KIND[m[2]] || SYSTEM_KIND.other, kind: m[2], label };
  if (label === 'stock system') return { display: 'The maker’s system', kind: 'stock', label };
  return { display: label, kind: 'other', label };
}

const isKept = (s) => /^kept/.test(s.outcome);
export const keptSystems = (plan) => plan.systems.filter(isKept).map((x) => systemView(x.system));
export const goneSystems = (plan) => plan.systems.filter((x) => !isKept(x)).map((x) => ({ ...systemView(x.system), outcome: x.outcome }));

const stepIndex = (plan, id) => plan.steps.findIndex((x) => x.id === id);

// One escape (on_failure or on_interruption) in words.
export function escapeWords(s, e, i) {
  const words = (LEAVES[e.leaves] || (() => e.leaves))(s.m.the, e.hardware);
  const [how, target] = (e.way_out || '').split(' ');
  if (how === 'transition') {
    const t = s.desc && s.desc.texts.transitions[target];
    return t ? `${words} ${t}` : words;
  }
  if (how === 'step' && /^installer-can-/.test(e.leaves)) {
    const j = stepIndex(s.plan, target);
    if (j === i) return words.replace(/\.$/, ', by doing this step again.');
    if (j > i) return words.replace(/\.$/, `, going on with step ${j + 1}.`);
  }
  return words;
}

export function stepViews(s) {
  const p = s.plan;
  return p.steps.map((st, i) => {
    const base = st.id.split('.')[0];
    const text = s.desc && s.desc.texts.steps[base];
    const [title, body] = splitSentence(text);
    const parts = p.partitions.filter((x) => x.steps.includes(st.id));
    const settings = p.boot_settings.filter((b) => b.steps.includes(st.id));
    return {
      id: st.id, n: i + 1, kind: st.kind, owed: !text, title: text ? title : `A step of kind “${st.kind.replace(/-/g, ' ')}”`, body,
      ponr: st.point_of_no_return, erase: parts.some((x) => x.loses_files), untried: st.writes && !st.tried,
      done: st.done === 'true', cable: st.on_failure.leaves === 'special-hardware',
      escape: escapeWords(s, st.on_failure, i), interruption: escapeWords(s, st.on_interruption, i),
      tech: [
        `${st.id}: ${st.kind}, in ${st.mode}`,
        ...parts.map((x) => `${x.partition}: ${x.actions.join(', ')}${x.size !== null ? ' (' + human(x.size) + ')' : ''}`),
        ...settings.map((b) => `${b.id}: ${b.actions.join(', ')}`),
        `if it fails: ${st.on_failure.leaves}${st.on_failure.hardware ? ' (' + st.on_failure.hardware + ')' : ''} via ${st.on_failure.way_out}`,
        `if cut off after it: ${st.on_interruption.leaves} via ${st.on_interruption.way_out}`,
      ],
    };
  });
}

const sum = (xs) => xs.reduce((a, x) => a + (x.size || 0), 0);
const parts = (n) => (n === 1 ? 'part' : 'parts');

// Keep / back up / replace / create, as lines.
export function fate(s) {
  const p = s.plan;
  const by = (f) => p.partitions.filter(f);
  const sizeOf = (xs) => {
    const known = xs.filter((x) => x.size !== null);
    if (!known.length) return 'size not known yet';
    return human(sum(known)) + (known.length < xs.length ? ' and more' : '');
  };
  const keep = keptSystems(p).map((x) => `${x.display}: kept, and it still starts`);
  if (p.keeps.device_data.length) keep.push(`Its device data: ${list(p.keeps.device_data.map((d) => DEVICE_DATA[d] || d.replace(/-/g, ' ')))}`);
  if (p.keeps.unit_values.length) keep.push(`Values unique to ${s.m.the}: ${p.keeps.unit_values.length}`);
  if (p.untouched_partitions) keep.push(`${p.untouched_partitions} other ${parts(p.untouched_partitions)} of the storage, not touched`);
  const backed = by((x) => x.actions.includes('backs-up'));
  const backup = [];
  if (backed.length) backup.push(`${backed.length} ${parts(backed.length)} of the storage: ${sizeOf(backed)}`);
  for (const b of p.boot_settings.filter((b) => b.actions.includes('back-up'))) backup.push(BOOT_SETTING[b.kind] || b.kind);
  const replace = [];
  const repl = by((x) => x.class === 'replace');
  if (repl.length) replace.push(`${repl.length} ${parts(repl.length)} of the storage, written anew`);
  for (const b of p.boot_settings.filter((b) => b.actions.includes('change'))) replace.push(BOOT_SETTING[b.kind] || b.kind);
  const rem = by((x) => x.class === 'remove');
  if (rem.length) replace.push(`${rem.length} ${parts(rem.length)} removed, with every file on them: ${sizeOf(rem)}`);
  const res = by((x) => x.class === 'resize');
  if (res.length) replace.push(`${res.length} ${parts(res.length)} made smaller and emptied: ${sizeOf(res)}`);
  for (const x of goneSystems(p)) replace.push(`${x.display}: ${x.outcome}`);
  const made = by((x) => x.class === 'create');
  const create = ['GlassineOS', ...(made.length ? [`${made.length} new ${parts(made.length)} of the storage: ${sizeOf(made)}`] : [])];
  return { keep, backup, replace: replace.length ? replace : ['Nothing'], create };
}

// The partitions whose files are lost, and the steps that do it.
export function erase(s) {
  const p = s.plan;
  const lost = p.partitions.filter((x) => x.loses_files);
  if (!lost.length) return null;
  const steps = [...new Set(lost.flatMap((x) => x.steps))].map((id) => stepIndex(p, id) + 1).filter((n) => n > 0).sort((a, b) => a - b);
  const total = lost.filter((x) => x.size !== null);
  return {
    parts: lost, steps, size: total.length ? human(sum(total)) : null,
    // removed with a system the person chose to replace: confirmed by typing its name instead
    withSystem: lost.every((x) => x.class === 'remove'),
    inBackup: lost.every((x) => x.actions.includes('backs-up')),
  };
}

export function stepList(ns) {
  return ns.length === 1 ? `step ${ns[0]}` : `steps ${list(ns.map(String))}`;
}

export function spaceRows(s) {
  const sp = s.plan.space;
  const rows = [];
  rows.push(['For the backup', `${human(sp.backup_bytes)}${sp.backup_unknown.length ? ', plus ' + list(sp.backup_unknown.map((x) => 'the ' + x)) + ', of a size not known yet' : ''}`]);
  rows.push(['For what the installer downloads', sp.artifacts_unknown.length && !sp.artifact_bytes ? 'Not known yet' : human(sp.artifact_bytes) + (sp.artifacts_unknown.length ? ' and more' : '')]);
  rows.push(['On this computer, in all', `${sp.exact ? '' : 'At least '}${human(sp.total_at_least)}`]);
  return rows;
}

// What the check saw, as rows a person reads (only what the report holds).
export function factRows(s) {
  const r = s.report, mch = r.machine, rows = [];
  if (r.recognition.name) rows.push(['Recognised as', r.recognition.name]);
  if (mch.cpu) rows.push(['Processor', [mch.cpu.vendor, mch.cpu.arch].filter(Boolean).join(', ')]);
  if (mch.board && (mch.board.vendor || mch.board.product)) rows.push(['Board', [mch.board.vendor, mch.board.product].filter(Boolean).join(' ')]);
  for (const d of mch.disks) rows.push(['Storage', `${human(d.size)} ${d.kind.toUpperCase()}${d.removable ? ', removable' : ''}${d.free ? ', ' + human(d.free) + ' free' : ''}`]);
  if (mch.systems.length) rows.push(['Runs now', list(mch.systems.map((x) => SYSTEM_KIND[x.kind] || x.kind))]);
  if (mch.firmware) rows.push(['Firmware', `${mch.firmware.kind.toUpperCase()}${mch.firmware.secure_boot ? ', Secure Boot ' + mch.firmware.secure_boot : ''}`]);
  rows.push(['Reached through', MODE[r.mode] || r.mode]);
  return rows;
}

export function techRows(s) {
  const r = s.report, mch = r.machine, rows = [];
  if (r.recognition.device) rows.push(['Description', r.recognition.device]);
  rows.push(['Mode', r.mode]);
  if (mch.usb) rows.push(['USB', `${mch.usb.vendor_id}:${mch.usb.product_id} (${mch.usb.interfaces.join(', ')})`]);
  if (mch.fastboot) {
    const v = mch.fastboot.values;
    for (const k of ['product', 'variant', 'hw-revision', 'secure', 'unlocked', 'current-slot']) if (v[k]) rows.push([`fastboot ${k}`, v[k]]);
    const sizes = Object.keys(v).filter((k) => k.startsWith('partition-size:') && v[k]);
    if (sizes.length) rows.push(['Partitions it named', String(sizes.length)]);
  }
  if (mch.android) for (const [k, v] of Object.entries(mch.android.values)) if (v) rows.push([k, v]);
  for (const d of mch.disks) rows.push(['Disk', `${d.kind}, ${human(d.size)}, ${d.table || 'no partition table'}, ${d.partitions.length} partitions`]);
  if (mch.firmware) rows.push(['Firmware boot entries', String(mch.firmware.boot_entries)]);
  return rows;
}

// The shareable report as lines (only the report's own fields, which hold no unit values).
export function reportLines(s) {
  const r = s.report;
  const lines = [`Recognition: ${r.recognition.result}${r.recognition.name ? ', ' + r.recognition.name : ''}`];
  for (const [k, v] of techRows(s)) lines.push(`${k}: ${v}`);
  for (const sg of r.recognition.signals) lines.push(`Signal of ${sg.description}: ${sg.test} is ${sg.result}${sg.seen ? ' (seen: ' + sg.seen + ')' : ''}`);
  for (const x of r.reasons) lines.push(`Reason: ${x}`);
  return lines;
}

export { cap };
