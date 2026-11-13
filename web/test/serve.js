// SPDX-License-Identifier: GPL-2.0-or-later
// A static server on 127.0.0.1, in one of two ways:
//   - the checkout: the pages and the test records they read with ?fixtures=1: web/, the core's
//     plan golden files (core/tests/golden/) and the device descriptions (devices/);
//   - a site: one directory as it would be published (--root DIR; make web-site assembles it),
//     nothing else.
// node web/test/serve.js [--root DIR] [PORT] serves until stopped; the tests start it themselves.
'use strict';
const http = require('http');
const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..', '..');
const ALLOWED = ['web/', 'core/tests/golden/', 'devices/'];
const TYPES = { '.wasm': 'application/wasm', '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.json': 'application/json', '.toml': 'text/plain; charset=utf-8', '.woff2': 'font/woff2', '.txt': 'text/plain; charset=utf-8' };

// root: a site's directory, or null for the checkout. Every request is passed to log, if given.
function serve(port = 0, { root = null, log = null } = {}) {
  const top = root ? path.resolve(root) : ROOT;
  const server = http.createServer((req, res) => {
    let rel;
    try { rel = decodeURIComponent(new URL(req.url, 'http://x').pathname).replace(/^\/+/, ''); } catch { rel = null; }
    if (log) log(rel);
    if (rel === '' && !root) { res.writeHead(302, { Location: '/web/' }); return res.end(); }
    if (rel !== null && (rel === '' || rel.endsWith('/'))) rel += 'index.html';
    const file = rel === null ? null : path.join(top, rel);
    if (!file || req.method !== 'GET' || rel.includes('\0') || !file.startsWith(top + path.sep)
      || (!root && !ALLOWED.some((a) => rel.startsWith(a))) || rel.split('/').some((p) => p.startsWith('.'))) {
      res.writeHead(404); return res.end();
    }
    fs.readFile(file, (err, data) => {
      if (err) { res.writeHead(404); return res.end(); }
      res.writeHead(200, { 'Content-Type': TYPES[path.extname(file)] || 'application/octet-stream', 'Cache-Control': 'no-store' });
      res.end(data);
    });
  });
  return new Promise((resolve) => server.listen(port, '127.0.0.1', () => resolve(server)));
}

module.exports = { serve };

if (require.main === module) {
  const args = process.argv.slice(2);
  const i = args.indexOf('--root');
  const root = i >= 0 ? args.splice(i, 2)[1] : null;
  serve(Number(args[0] || 8790), { root }).then((s) => console.log(`http://127.0.0.1:${s.address().port}/${root ? '' : 'web/'}`));
}
