// SPDX-License-Identifier: GPL-2.0-or-later
// A static server for the pages and the records they read: web/, the core's plan golden files
// (core/tests/golden/) and the device descriptions (devices/), from this checkout, on
// 127.0.0.1. node web/test/serve.js [PORT] serves until stopped; the tests start it themselves.
'use strict';
const http = require('http');
const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..', '..');
const ALLOWED = ['web/', 'core/tests/golden/', 'devices/'];
const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.json': 'application/json', '.toml': 'text/plain; charset=utf-8', '.woff2': 'font/woff2', '.txt': 'text/plain; charset=utf-8' };

function serve(port = 0) {
  const server = http.createServer((req, res) => {
    let rel;
    try { rel = decodeURIComponent(new URL(req.url, 'http://x').pathname).replace(/^\/+/, ''); } catch { rel = null; }
    if (rel === '') { res.writeHead(302, { Location: '/web/' }); return res.end(); }
    if (rel !== null && rel.endsWith('/')) rel += 'index.html';
    const file = rel === null ? null : path.join(ROOT, rel);
    if (!file || req.method !== 'GET' || rel.includes('\0') || !file.startsWith(ROOT + path.sep)
      || !ALLOWED.some((a) => rel.startsWith(a)) || rel.split('/').some((p) => p.startsWith('.'))) {
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
  serve(Number(process.argv[2] || 8790)).then((s) => console.log(`http://127.0.0.1:${s.address().port}/web/`));
}
