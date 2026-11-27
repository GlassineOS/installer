// SPDX-License-Identifier: GPL-2.0-or-later
// The PC installer drive (spec 005): which published drive to offer, and the check of the file
// the person downloaded. Everything that knows how GlassineOS/images publishes it is here.
//
// Where the facts come from. GlassineOS/images publishes each PC release as a GitHub Release
// tagged uefi-pc-<base-id> (its docs/artifacts.md, "Published PC releases"); the drive image is
// one more file of that release (images spec 004). A page on another site can read GitHub's list
// of releases (api.github.com answers with Access-Control-Allow-Origin: *), but not the files
// themselves: their download redirects to release-assets.githubusercontent.com, which sends no
// such header, so a fetch of release.json, SHA256SUMS or the image fails in the browser. The size
// and the SHA-256 therefore come from the list, which names both for every file (`size`, and
// `digest`: "sha256:<hex>", computed by GitHub from the uploaded bytes); the browser downloads the
// file itself (a link, which needs no such header) and the person picks it for the check.
//
// The drive file's name. Images spec 004 makes glassine-drive-<base-id>.img; how it is published
// (that name, or packed, since a raw drive is near GitHub's 2 GiB per file) is not specified yet.
// This module takes glassine-drive-<base-id>.img, optionally packed with .xz, .zst or .gz; the
// screens name the unpacking for dd from the suffix.

export const OWNER_REPO = 'GlassineOS/images';
export const RELEASES_URL = `https://api.github.com/repos/${OWNER_REPO}/releases?per_page=30`;
export const RELEASES_PAGE = `https://github.com/${OWNER_REPO}/releases`;
const TAG = /^uefi-pc-(.+)$/;
const IMAGE = /^glassine-drive-(.+)\.img(?:\.(xz|zst|gz))?$/;
const DIGEST = /^sha256:([0-9a-f]{64})$/;

// The drive to offer from GitHub's list of releases (an array, as the API gives it):
//   { state: 'found', drive: { tag, base, published, name, url, size, sha256, packed } }
//   { state: 'none' }   no published PC release carries a drive whose size and SHA-256 it names
// The newest by publication time: the API's order is not that (it sorts by the tag's creation).
export function pickDrive(list) {
  if (!Array.isArray(list)) throw new Error('the list of releases isn’t a list');
  const found = [];
  for (const r of list) {
    if (!r || r.draft || !r.published_at) continue;
    const t = TAG.exec(r.tag_name || '');
    if (!t) continue;
    for (const a of r.assets || []) {
      const m = IMAGE.exec(a.name || '');
      const d = DIGEST.exec(a.digest || '');
      if (!m || !d || !Number.isSafeInteger(a.size) || a.size <= 0 || !a.browser_download_url) continue;
      found.push({ tag: r.tag_name, base: t[1], published: r.published_at, name: a.name, url: a.browser_download_url, size: a.size, sha256: d[1], packed: m[2] || null });
    }
  }
  if (!found.length) return { state: 'none' };
  found.sort((x, y) => Date.parse(y.published) - Date.parse(x.published));
  return { state: 'found', drive: found[0] };
}

// Reads the list and picks the drive. A list that can't be read rejects with an error whose kind
// is 'offline' (no answer) or 'refused' (an answer that isn't the list: GitHub's rate limit for
// a computer without an account is 60 lists an hour).
export async function findDrive(get = fetch) {
  let r;
  try {
    r = await get(RELEASES_URL, { headers: { Accept: 'application/vnd.github+json' } });
  } catch (e) {
    throw Object.assign(new Error(String((e && e.message) || e)), { kind: 'offline' });
  }
  if (!r.ok) throw Object.assign(new Error(`GitHub answered ${r.status}`), { kind: 'refused', status: r.status });
  let list;
  try { list = await r.json(); } catch { throw Object.assign(new Error('the answer isn’t a list of releases'), { kind: 'refused' }); }
  return pickDrive(list);
}

// Checks a file the person picked against the drive: its size, then its SHA-256, read in
// pieces. onProgress(bytes) as it reads; stop() (from start) ends it early. Resolves with
//   { result: 'match', sha256 } | { result: 'short' | 'long', size } | { result: 'differs', sha256 }
//   | { result: 'stopped' }
// and rejects when the file can't be read.
export function checkFile(file, drive, onProgress = () => {}, Hash) {
  let stopped = false, reader = null;
  const stop = () => { stopped = true; if (reader) reader.cancel().catch(() => {}); };
  const done = (async () => {
    if (file.size !== drive.size) return { result: file.size < drive.size ? 'short' : 'long', size: file.size };
    const h = new Hash();
    reader = file.stream().getReader();
    let read = 0;
    for (;;) {
      const { value, done: end } = await reader.read();
      if (stopped) return { result: 'stopped' };
      if (end) break;
      h.update(value);
      read += value.length;
      onProgress(read);
    }
    const sha256 = h.hex();
    return { result: sha256 === drive.sha256 ? 'match' : 'differs', sha256 };
  })();
  return { done, stop };
}

// The computer's system, for the writing steps shown first: windows | macos | linux.
export function systemOf(nav = navigator) {
  const p = ((nav.userAgentData && nav.userAgentData.platform) || nav.platform || nav.userAgent || '').toLowerCase();
  if (p.includes('win')) return 'windows';
  if (p.includes('mac')) return 'macos';
  return 'linux';
}
