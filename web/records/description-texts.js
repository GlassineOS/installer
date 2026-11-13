// SPDX-License-Identifier: GPL-2.0-or-later
// Reads what the screens need from a device description file (format 1, TOML): the top-level
// `id`, `name` and `backend`, the `[texts]` tables, and the string fields `id`, `kind`, `from`,
// `to` and `by` of each `[[modes]]` and `[[transitions]]` entry. The core in the browser hands over
// the source of the copy it carries; with fixtures, it is the repository's file. It reads only that subset: inside
// `[texts.*]` every line must be `key = "string"`, anything else there is an error, and every
// other table is skipped.

const KEY = String.raw`(?:[A-Za-z0-9_-]+|"(?:[^"\\]|\\.)*")`;
const STRING = String.raw`"(?:[^"\\]|\\.)*"`;
const PAIR = new RegExp(String.raw`^(${KEY})\s*=\s*(${STRING})\s*(?:#.*)?$`);
const TABLE = /^\[([A-Za-z0-9_.-]+)\]\s*(?:#.*)?$/;

const unquote = (s) => (s.startsWith('"') ? JSON.parse(s) : s);

export function descriptionTexts(source, file = 'description') {
  const out = { id: null, name: null, backend: null, modes: [], transitions: [], texts: { name: null, modes: {}, transitions: {}, choices: {}, options: {}, steps: {} } };
  let table = ''; // '' = top level, null = inside an array of tables
  let entry = null; // the [[modes]] or [[transitions]] entry being read
  const lines = source.split('\n');
  lines.forEach((raw, i) => {
    const line = raw.trim();
    if (line === '' || line.startsWith('#')) return;
    const arr = /^\[\[([A-Za-z0-9_.-]+)\]\]\s*(?:#.*)?$/.exec(line);
    if (arr) {
      entry = arr[1] === 'modes' || arr[1] === 'transitions' ? {} : null;
      if (entry) out[arr[1]].push(entry);
      table = null;
      return;
    }
    const t = TABLE.exec(line);
    if (t) { table = t[1]; entry = null; return; }
    if (table === null) {
      const m = entry && PAIR.exec(line);
      if (m && ['id', 'kind', 'from', 'to', 'by'].includes(unquote(m[1]))) entry[unquote(m[1])] = JSON.parse(m[2]);
      return;
    }
    const inTexts = table === 'texts' || table.startsWith('texts.');
    if (table !== '' && !inTexts) return;
    const m = PAIR.exec(line);
    if (!m) {
      if (inTexts) throw new Error(`${file}:${i + 1}: [${table}] holds something other than key = "string"`);
      return; // a top-level value that isn't a one-line string (arrays, numbers)
    }
    const key = unquote(m[1]), value = JSON.parse(m[2]);
    if (table === '') {
      if (key === 'id' || key === 'name' || key === 'backend') out[key] = value;
    } else if (table === 'texts') {
      if (key === 'name') out.texts.name = value;
    } else {
      const section = table.slice('texts.'.length);
      if (!(section in out.texts) || section === 'name') throw new Error(`${file}:${i + 1}: unknown texts table [${table}]`);
      out.texts[section][key] = value;
    }
  });
  if (!out.id || !out.backend) throw new Error(`${file}: no id or backend`);
  return out;
}
