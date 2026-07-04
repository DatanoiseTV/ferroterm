// Headless render regression test. Serves the package, drives web/test/render.html
// in headless Chrome (software GL for determinism), and asserts semantic pixel
// colours per renderer plus same-renderer determinism. Exits non-zero on failure.
//
//   node test/run.mjs            (from the web/ directory)
//   CHROME_BIN=/path/to/chrome node test/run.mjs
//
// Requires Node 18+ (global fetch/WebSocket) and a Chrome/Chromium build.

import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { dirname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const WEB_ROOT = join(HERE, '..'); // serve the web/ package root
const PORT = 8123;
const DEBUG_PORT = 9315;
const TOL = 42; // per-channel colour tolerance (AA / atlas rounding)

const MIME = {
  '.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript',
  '.css': 'text/css', '.wasm': 'application/wasm', '.json': 'application/json',
};

function findChrome() {
  if (process.env.CHROME_BIN) return process.env.CHROME_BIN;
  const candidates = [
    '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    '/Applications/Chromium.app/Contents/MacOS/Chromium',
    '/usr/bin/google-chrome', '/usr/bin/google-chrome-stable',
    '/usr/bin/chromium', '/usr/bin/chromium-browser',
  ];
  for (const c of candidates) if (existsSync(c)) return c;
  throw new Error('Chrome not found; set CHROME_BIN to a Chrome/Chromium binary');
}

function serve() {
  const server = createServer(async (req, res) => {
    try {
      const path = normalize(decodeURIComponent(req.url.split('?')[0]));
      const file = join(WEB_ROOT, path);
      if (!file.startsWith(WEB_ROOT)) { res.writeHead(403).end(); return; }
      const body = await readFile(file);
      const ext = file.slice(file.lastIndexOf('.'));
      res.writeHead(200, { 'content-type': MIME[ext] || 'application/octet-stream' });
      res.end(body);
    } catch {
      res.writeHead(404).end('not found');
    }
  });
  return new Promise((r) => server.listen(PORT, () => r(server)));
}

async function cdp(wsUrl) {
  const ws = new WebSocket(wsUrl);
  await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
  let id = 0;
  const pending = new Map();
  ws.onmessage = (e) => {
    const m = JSON.parse(e.data);
    if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); }
  };
  const send = (method, params = {}) =>
    new Promise((res) => { const i = ++id; pending.set(i, res); ws.send(JSON.stringify({ id: i, method, params })); });
  return { send, close: () => ws.close() };
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const close = (a, b) => Math.abs(a[0] - b[0]) <= TOL && Math.abs(a[1] - b[1]) <= TOL && Math.abs(a[2] - b[2]) <= TOL;

async function main() {
  const chromeBin = findChrome();
  const server = await serve();
  const chrome = spawn(chromeBin, [
    '--headless', '--disable-gpu', '--use-gl=swiftshader', '--enable-unsafe-swiftshader',
    '--no-sandbox', `--remote-debugging-port=${DEBUG_PORT}`, 'about:blank',
  ], { stdio: 'ignore' });

  const cleanup = () => { try { chrome.kill(); } catch {} try { server.close(); } catch {} };

  try {
    // Wait for the debugging endpoint.
    let target;
    for (let i = 0; i < 50; i++) {
      try {
        const list = await (await fetch(`http://localhost:${DEBUG_PORT}/json`)).json();
        target = list.find((t) => t.type === 'page');
        if (target) break;
      } catch {}
      await sleep(100);
    }
    if (!target) throw new Error('Chrome devtools endpoint did not come up');

    const { send, close: closeWs } = await cdp(target.webSocketDebuggerUrl);
    await send('Runtime.enable');
    await send('Page.navigate', { url: `http://localhost:${PORT}/test/render.html` });

    let done = false;
    for (let i = 0; i < 100; i++) {
      const r = await send('Runtime.evaluate', { expression: '!!window.__done', returnByValue: true });
      if (r.result?.result?.value) { done = true; break; }
      await sleep(100);
    }
    if (!done) throw new Error('fixture did not finish (window.__done never set)');

    const err = (await send('Runtime.evaluate', { expression: 'window.__error || null', returnByValue: true })).result.result.value;
    if (err) throw new Error('fixture error: ' + err);

    const data = (await send('Runtime.evaluate', { expression: 'window.__results', returnByValue: true })).result.result.value;
    closeWs();

    const T = data.theme;
    const expected = {
      redText: T.red, blueBg: T.blue, defaultBg: T.bg,
      inverseBg: T.fg, trueColor: [255, 128, 0], greenWide: T.green,
    };

    let failures = 0;
    for (const renderer of ['canvas', 'webgl']) {
      const r = data.results[renderer];
      console.log(`\n  ${renderer} (${r.name})`);
      for (const [key, exp] of Object.entries(expected)) {
        const got = r.checks[key];
        const ok = close(got, exp);
        console.log(`    ${ok ? 'PASS' : 'FAIL'}  ${key.padEnd(11)} got [${got}] expected ~[${exp}]`);
        if (!ok) failures++;
      }
      const detOk = r.determinismMaxDiff === 0;
      console.log(`    ${detOk ? 'PASS' : 'FAIL'}  determinism (max channel diff ${r.determinismMaxDiff})`);
      if (!detOk) failures++;
    }

    // Incremental (dirty-row) output must match a full re-render exactly.
    console.log('\n  webgl incremental parity (vs full re-render)');
    for (const [key, d] of Object.entries(data.incremental)) {
      const ok = d === 0;
      console.log(`    ${ok ? 'PASS' : 'FAIL'}  ${key.padEnd(15)} max channel diff ${d}`);
      if (!ok) failures++;
    }

    console.log(`\n  ${failures === 0 ? 'ALL PASS' : failures + ' FAILURE(S)'}\n`);
    cleanup();
    process.exit(failures === 0 ? 0 : 1);
  } catch (e) {
    console.error('  ERROR:', e.message);
    cleanup();
    process.exit(2);
  }
}

main();
