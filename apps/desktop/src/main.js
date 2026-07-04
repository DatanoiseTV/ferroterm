// Ferroterm desktop front-end: tabbed terminals wired to native PTYs, plus a
// battery + performance HUD. Uses the global Tauri API (withGlobalTauri).

import { Ferroterm } from './ferroterm/src/index.js';

const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;

const stack = document.getElementById('stack');
const tabsEl = document.getElementById('tabs');
const tabs = new Map(); // id -> { term, pane, tabEl, labelEl, title, bytes }
let activeId = null;
let counter = 0;
let bytesThisSecond = 0;

// --- global PTY event routing ---------------------------------------------

await listen('pty:data', (e) => {
  const { id, bytes } = e.payload;
  const t = tabs.get(id);
  if (t) {
    const u8 = new Uint8Array(bytes);
    t.term.write(u8);
    bytesThisSecond += u8.length;
  }
});

await listen('pty:exit', (e) => {
  const id = e.payload;
  if (tabs.has(id)) closeTab(id);
});

// --- tab management --------------------------------------------------------

async function newTab() {
  const id = `t${++counter}`;
  const pane = document.createElement('div');
  pane.className = 'term-pane';
  stack.appendChild(pane);

  const term = await Ferroterm.create(pane, {
    renderer: 'webgl',
    fontSize: 13,
    scrollback: 5000,
    copyOnSelect: true,
  });

  const tabEl = document.createElement('div');
  tabEl.className = 'tab';
  const labelEl = document.createElement('span');
  labelEl.className = 'label';
  labelEl.textContent = 'shell';
  const closeEl = document.createElement('span');
  closeEl.className = 'close';
  closeEl.textContent = '×';
  tabEl.append(labelEl, closeEl);
  tabsEl.appendChild(tabEl);

  const entry = { id, term, pane, tabEl, labelEl, title: 'shell' };
  tabs.set(id, entry);

  tabEl.addEventListener('mousedown', (e) => {
    if (e.target === closeEl) return;
    activate(id);
  });
  closeEl.addEventListener('click', (e) => {
    e.stopPropagation();
    closeTab(id);
  });

  activate(id);
  term.fit();

  // Spawn the PTY at the terminal's size, then wire I/O both ways.
  await invoke('pty_spawn', { id, cols: term.cols, rows: term.rows });
  term.onData((bytes) => {
    invoke('pty_write', { id, data: Array.from(bytes) });
  });
  term.onResize((cols, rows) => {
    invoke('pty_resize', { id, cols, rows });
  });
  term.onTitleChange((t) => {
    entry.title = t || 'shell';
    labelEl.textContent = entry.title;
    labelEl.title = entry.title;
  });

  term.focus();
  return entry;
}

function activate(id) {
  const entry = tabs.get(id);
  if (!entry) return;
  for (const t of tabs.values()) {
    t.pane.classList.toggle('active', t.id === id);
    t.tabEl.classList.toggle('active', t.id === id);
  }
  activeId = id;
  entry.term.fit();
  entry.term.focus();
  updateRendererHud();
}

async function closeTab(id) {
  const entry = tabs.get(id);
  if (!entry) return;
  try {
    await invoke('pty_kill', { id });
  } catch {
    /* already gone */
  }
  entry.term.dispose();
  entry.pane.remove();
  entry.tabEl.remove();
  tabs.delete(id);

  if (activeId === id) {
    const next = tabs.keys().next().value;
    if (next) activate(next);
    else newTab(); // never leave zero tabs
  }
}

document.getElementById('new-tab').addEventListener('click', () => newTab());

// Keyboard: Cmd/Ctrl+T new, Cmd/Ctrl+W close, Cmd/Ctrl+1..9 switch.
window.addEventListener('keydown', (e) => {
  const meta = e.metaKey || e.ctrlKey;
  if (!meta) return;
  if (e.key === 't') {
    newTab();
    e.preventDefault();
  } else if (e.key === 'w') {
    if (activeId) closeTab(activeId);
    e.preventDefault();
  } else if (e.key >= '1' && e.key <= '9') {
    const arr = [...tabs.keys()];
    const idx = +e.key - 1;
    if (arr[idx]) {
      activate(arr[idx]);
      e.preventDefault();
    }
  }
});

// --- HUD -------------------------------------------------------------------

const hudFps = document.querySelector('#hud-fps b');
const hudTput = document.querySelector('#hud-tput b');
const hudRenderer = document.getElementById('hud-renderer');
const hudBattery = document.getElementById('hud-battery');

function updateRendererHud() {
  const t = tabs.get(activeId);
  hudRenderer.textContent = t ? t.term.rendererName : '–';
}

// FPS meter.
let frames = 0;
let lastFps = performance.now();
(function loop(now) {
  frames++;
  if (now - lastFps >= 500) {
    hudFps.textContent = Math.round((frames * 1000) / (now - lastFps));
    frames = 0;
    lastFps = now;
  }
  requestAnimationFrame(loop);
})(performance.now());

// Throughput (KB/s), sampled once a second.
setInterval(() => {
  hudTput.textContent = Math.round(bytesThisSecond / 1024);
  bytesThisSecond = 0;
}, 1000);

// Battery, polled every 5s.
function fmtTime(s) {
  if (!s) return '';
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  return h ? `${h}h ${m}m` : `${m}m`;
}
async function pollBattery() {
  try {
    const b = await invoke('battery_status');
    if (!b.present || b.percent == null) {
      hudBattery.textContent = '';
      return;
    }
    const pct = Math.round(b.percent);
    const icon = b.charging ? '⚡' : '';
    const time = fmtTime(b.seconds_remaining);
    hudBattery.innerHTML = `${icon}<b>${pct}%</b>${time ? ' · ' + time : ''}`;
    hudBattery.classList.toggle('low', pct <= 20 && !b.charging);
  } catch {
    hudBattery.textContent = '';
  }
}
pollBattery();
setInterval(pollBattery, 5000);

// --- boot ------------------------------------------------------------------

await newTab();
