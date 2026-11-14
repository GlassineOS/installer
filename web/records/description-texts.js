// SPDX-License-Identifier: GPL-2.0-or-later
// Reads what the screens need from a device description file (format 1, TOML): the top-level
// `id`, `name` and `backend`, the `[texts]` tables, the string fields `id`, `kind`, `from`,
// `to`, `by` and `needs` of each `[[modes]]` and `[[transitions]]` entry, and of each
// `[[supplied_files]]` entry (format 3) its `id`, `what`, `usual_name`, the one-line inline
// table `matches` (string values) and its `signature`'s `boot_rom`. The checksums the core
// checks a file against are not read here: the check is the core's (records.suppliedCheck). The core in the browser hands over
// the source of the copy it carries; with fixtures, it is the repository's file. It reads only that subset: inside
// `[texts.*]` every line must be `key = "string"`, anything else there is an error, and every
// other table is skipped. A supplied file's texts (format 3) are one table each,
// `[texts.supplied_files.<key>]`, read into `texts.supplied_files[key]`.

const KEY = String.raw`(?:[A-Za-z0-9_-]+|"(?:[^"\\]|\\.)*")`;
const STRING = String.raw`"(?:[^"\\]|\\.)*"`;
const PAIR = new RegExp(String.raw`^(${KEY})\s*=\s*(${STRING})\s*(?:#.*)?$`);
const TABLE = /^\[([A-Za-z0-9_.-]+)\]\s*(?:#.*)?$/;

const unquote = (s) => (s.startsWith('"') ? JSON.parse(s) : s);
const INLINE = /^([A-Za-z0-9_-]+)\s*=\s*\{(.*)\}\s*(?:#.*)?$/;
// The string values at the top of a one-line inline table: `{ a = "x", b = "y" }`; nested
// tables and other values are skipped.
function inlineStrings(body) {
  const out = {}, re = new RegExp(String.raw`(?:^|,)\s*(${KEY})\s*=\s*(${STRING})(?=\s*(?:,|$))`, 'g');
  for (const m of body.replace(/\{[^{}]*\}/g, '{}').matchAll(re)) out[unquote(m[1])] = JSON.parse(m[2]);
  return out;
}

export function descriptionTexts(source, file = 'description') {
  const out = { id: null, name: null, backend: null, modes: [], transitions: [], supplied_files: [], texts: { name: null, modes: {}, transitions: {}, choices: {}, options: {}, steps: {}, supplied_files: {} } };
  let table = ''; // '' = top level, null = inside an array of tables
  let entry = null; // the [[modes]], [[transitions]] or [[supplied_files]] entry being read
  const ENTRIES = { modes: ['id', 'kind', 'from', 'to', 'by', 'needs'], transitions: ['id', 'kind', 'from', 'to', 'by', 'needs'], supplied_files: ['id', 'what', 'usual_name'] };
  let fields = null;
  const lines = source.split('\n');
  lines.forEach((raw, i) => {
    const line = raw.trim();
    if (line === '' || line.startsWith('#')) return;
    const arr = /^\[\[([A-Za-z0-9_.-]+)\]\]\s*(?:#.*)?$/.exec(line);
    if (arr) {
      // [[supplied_files.known]] and the like are sub-tables of the entry: none of it is read.
      entry = arr[1] in ENTRIES ? {} : null;
      fields = entry ? ENTRIES[arr[1]] : null;
      if (entry) out[arr[1]].push(entry);
      table = null;
      return;
    }
    const t = TABLE.exec(line);
    if (t) { table = t[1]; entry = null; return; }
    if (table === null) {
      if (!entry) return;
      const m = PAIR.exec(line);
      if (m && fields.includes(unquote(m[1]))) entry[unquote(m[1])] = JSON.parse(m[2]);
      if (fields === ENTRIES.supplied_files) {
        const t = INLINE.exec(line);
        if (t && t[1] === 'matches') entry.matches = inlineStrings(t[2]);
        if (t && t[1] === 'signature') entry.boot_rom = inlineStrings(t[2]).boot_rom || null;
      }
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
    } else if (table.startsWith('texts.supplied_files.')) {
      const which = table.slice('texts.supplied_files.'.length);
      (out.texts.supplied_files[which] ??= {})[key] = value;
    } else {
      const section = table.slice('texts.'.length);
      if (!(section in out.texts) || section === 'name' || section === 'supplied_files') throw new Error(`${file}:${i + 1}: unknown texts table [${table}]`);
      out.texts[section][key] = value;
    }
  });
  if (!out.id || !out.backend) throw new Error(`${file}: no id or backend`);
  return out;
}
