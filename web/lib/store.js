// SPDX-License-Identifier: GPL-2.0-or-later
// Where an emergency read stores its images in the browser (spec 004 FR-010): a folder the
// person picked (File System Access API), with the command line's files and rules
// (cli/src/backup.rs FileEdge):
//
//   NAME.img.gz.partial      an image while it is written and read back (gzip)
//   NAME.img.gz              its final name, given only after both reads matched and the stored
//                            copy read back the same (the core calls keep)
//   NAME.img.gz.incomplete   a copy that failed a check: never taken for a backup
//   manifest.json            stored last, whole or not at all (written as manifest.json.partial,
//                            then renamed)
//
// Nothing is overwritten: the images go into a new folder inside the one picked, and no name is
// written that exists. Nothing goes to the browser's own storage (origin-private files,
// IndexedDB) or to a download: an image is as big as the device's storage.
//
// The core drives it (wasm emergency_read, its ImageStore): every step returns a promise but
// write, stop and stopped; write queues the bytes into a gzip stream to the file and throws a failure of an
// earlier write. ready() resolves once the queued bytes are under QUEUE_MAX: the core waits for
// it before each transfer from the device, so a slow disk slows the read instead of filling
// memory.
//
// A browser writes a file through a temporary file of its own and puts the bytes under the
// file's name only when it is closed: a page closed while an image is written leaves the
// .partial name with what was closed before (nothing, for an image being written), never a
// final name.

const QUEUE_MAX = 16 << 20;
const MANIFEST = 'manifest.json';

// Whether this browser can store a backup into a folder: the folder picker, gzip both ways,
// and renaming a file in that folder (Chromium 111 and later).
export function storeSupported() {
  return typeof globalThis.showDirectoryPicker === 'function'
    && typeof globalThis.CompressionStream === 'function'
    && typeof globalThis.DecompressionStream === 'function'
    && typeof globalThis.FileSystemFileHandle === 'function'
    && typeof globalThis.FileSystemFileHandle.prototype.move === 'function';
}

// Asks the person for the folder (call from a click). Rejects with AbortError when the picker
// was closed.
export function pickFolder() {
  return globalThis.showDirectoryPicker({ id: 'glassine-backup', mode: 'readwrite' });
}

const two = (n) => String(n).padStart(2, '0');

// The new folder's name: the device's description and the time, e.g.
// onyx-noteair-backup-2026-11-14-153012.
export function folderName(device, at = new Date()) {
  return `${device}-backup-${at.getFullYear()}-${two(at.getMonth() + 1)}-${two(at.getDate())}-${two(at.getHours())}${two(at.getMinutes())}${two(at.getSeconds())}`;
}

async function exists(dir, name) {
  try {
    await dir.getFileHandle(name);
    return true;
  } catch (e) {
    if (e && e.name === 'NotFoundError') return false;
    if (e && e.name === 'TypeMismatchError') return true; // a folder of that name
    throw e;
  }
}

// Creates the store's new folder `name` inside `parent` (the picked folder): it must not exist.
export async function newFolder(parent, name) {
  let taken = true;
  try {
    await parent.getDirectoryHandle(name);
  } catch (e) {
    if (e && e.name === 'NotFoundError') taken = false;
    else if (!(e && e.name === 'TypeMismatchError')) throw e;
  }
  if (taken) throw new Error(`${name} exists already in the folder you chose; a backup never goes into an existing folder`);
  return parent.getDirectoryHandle(name, { create: true });
}

// The store over `folder`, a new, empty folder, or a function that makes it when the first image
// begins (so that a run that never gets that far leaves no folder). `on` gets what happens, for the screen:
//   { kind: 'begin', name } · { kind: 'saved', name, done, size } · { kind: 'checking', name, done }
//   { kind: 'kept', name, file } · { kind: 'incomplete', name, file } · { kind: 'manifest' }
//   { kind: 'note', line }
export function folderStore(folder, on = () => {}) {
  let made = null;
  const at = () => (made = made || Promise.resolve(typeof folder === 'function' ? folder() : folder));
  let cur = null; // the image being written: { name, writer, piping, error }
  let back = null; // the image being read back: { name, reader, rest, done }
  let pending = 0;
  let waiters = [];
  let stopped = false;
  const wake = () => {
    if (pending > QUEUE_MAX && !(cur && cur.error)) return;
    const w = waiters;
    waiters = [];
    for (const f of w) f();
  };
  const file = (name) => `${name}.img.gz`;
  const partial = (name) => `${file(name)}.partial`;
  const fresh = async (...names) => {
    for (const n of names) if (await exists(await at(), n)) throw new Error(`${n} exists already`);
  };
  const stopReading = () => {
    if (back) back.reader.cancel().catch(() => {});
    back = null;
  };

  return {
    async begin(name) {
      if (cur) throw new Error(`${cur.name} is still being written`);
      await fresh(file(name), partial(name));
      const h = await (await at()).getFileHandle(partial(name), { create: true });
      const out = await h.createWritable({ keepExistingData: false });
      const gz = new CompressionStream('gzip');
      const c = { name, writer: gz.writable.getWriter(), error: null };
      c.piping = gz.readable.pipeTo(out).catch((e) => { c.error = c.error || e; wake(); throw e; });
      c.piping.catch(() => {});
      cur = c;
      pending = 0;
      on({ kind: 'begin', name });
    },

    // The core's bytes are a view into its memory: copied before they are queued.
    write(data) {
      if (!cur) throw new Error('no image is being written');
      if (cur.error) throw cur.error;
      const c = cur, b = data.slice();
      pending += b.length;
      c.writer.write(b).then(
        () => { pending -= b.length; wake(); },
        (e) => { pending -= b.length; c.error = c.error || e; wake(); },
      );
    },

    ready() {
      if (pending <= QUEUE_MAX || (cur && cur.error)) return Promise.resolve();
      return new Promise((r) => waiters.push(r));
    },

    // Every byte written and the file closed: only now are they under its name.
    async end(name) {
      if (!cur || cur.name !== name) throw new Error(`${name} isn't being written`);
      const c = cur;
      try {
        if (c.error) throw c.error;
        await c.writer.close();
        await c.piping;
        if (c.error) throw c.error;
      } finally {
        cur = null;
        pending = 0;
        wake();
      }
    },

    async reopen(name) {
      stopReading();
      const f = await (await (await at()).getFileHandle(partial(name))).getFile();
      back = { name, reader: f.stream().pipeThrough(new DecompressionStream('gzip')).getReader(), rest: null, done: 0 };
    },

    // Up to `max` bytes of the copy reopened last; none at its end.
    async read(max) {
      if (!back) throw new Error('no image is being read back');
      while (!back.rest || back.rest.length === 0) {
        const { done, value } = await back.reader.read();
        if (done) return new Uint8Array(0);
        back.rest = value;
      }
      const n = Math.min(max, back.rest.length);
      const out = back.rest.slice(0, n);
      back.rest = back.rest.subarray(n);
      back.done += n;
      // Told every 4 MiB (and at the start): a read-back of a whole disk is many small reads.
      if (back.done - (back.told || 0) >= 4 << 20 || !back.told) { back.told = back.done; on({ kind: 'checking', name: back.name, done: back.done }); }
      return out;
    },

    async keep(name) {
      stopReading();
      await fresh(file(name));
      await (await (await at()).getFileHandle(partial(name))).move(file(name));
      on({ kind: 'kept', name, file: file(name) });
      return file(name);
    },

    // Whatever was written is kept, under a name no backup is read by.
    async incomplete(name) {
      if (cur && cur.name === name) {
        const c = cur;
        cur = null;
        pending = 0;
        wake();
        try { await c.writer.close(); await c.piping; } catch { try { await c.writer.abort(); } catch { /* gone already */ } }
      }
      stopReading();
      const to = `${file(name)}.incomplete`;
      let from = null;
      for (const n of [partial(name), file(name)]) if (await exists(await at(), n)) { from = n; break; }
      if (!from) throw new Error(`${file(name)}: no copy to mark`);
      await fresh(to);
      await (await (await at()).getFileHandle(from)).move(to);
      on({ kind: 'incomplete', name, file: to });
      return to;
    },

    async manifest(text) {
      const tmp = `${MANIFEST}.partial`;
      await fresh(MANIFEST, tmp);
      const h = await (await at()).getFileHandle(tmp, { create: true });
      const w = (await h.createWritable({ keepExistingData: false })).getWriter();
      await w.write(new TextEncoder().encode(text));
      await w.close();
      await h.move(MANIFEST);
      on({ kind: 'manifest' });
    },

    // The person stopped the run: the core asks the device for nothing more but its restart,
    // marks the image being read incomplete and stores the manifest.
    stop() { if (!stopped) { stopped = true; on({ kind: 'note', line: 'stopped by you: the device is asked to restart' }); } },
    stopped() { return stopped; },

    progress(name, done, size) { on({ kind: 'saved', name, done, size }); },
    note(line) { on({ kind: 'note', line }); },
  };
}
