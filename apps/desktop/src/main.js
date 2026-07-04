// Ferroterm desktop front-end: tabbed, split-pane terminals wired to native
// PTYs, with a battery + performance HUD, multi-window support, find, and font
// zoom.
//
// Scaling: a session's ferroterm *engine* (WASM core) is always alive and fed
// by its PTY, but its *view* (renderer + WebGL context) is attached only while
// its pane is on screen. Switching away from a tab detaches its panes' views,
// so hundreds of tabs cost almost nothing and never exhaust WebGL contexts.

import { Ferroterm } from './ferroterm/src/index.js';

const invoke = window.__TAURI__.core.invoke;
const listen = window.__TAURI__.event.listen;
const WebviewWindow = window.__TAURI__?.webviewWindow?.WebviewWindow;

const stack = document.getElementById('stack');
const tabsEl = document.getElementById('tabs');

// Per-window prefix keeps PTY ids unique across windows (events are broadcast to
// all windows; each routes only ids it owns).
const WIN = 'w' + Math.random().toString(36).slice(2, 8);
let counter = 0;
let fontSize = 13;
let bytesThisSecond = 0;

const sessions = new Map(); // id -> { id, term, title }
const tabs = []; // { id, root, container, tabEl, labelEl }
let activeTabId = null;
let activeLeaf = null;

// --- PTY event routing -----------------------------------------------------

await listen('pty:data', (e) => {
  const { id, bytes } = e.payload;
  const s = sessions.get(id);
  if (s) {
    const u8 = new Uint8Array(bytes);
    s.term.write(u8);
    bytesThisSecond += u8.length;
  }
});
await listen('pty:exit', (e) => {
  const leaf = findLeafBySession(e.payload);
  if (leaf) closePane(leaf);
});

// --- sessions & panes ------------------------------------------------------

function newSession() {
  const id = `${WIN}-${++counter}`;
  const term = new Ferroterm({
    renderer: 'webgl',
    fontSize,
    scrollback: 5000,
    copyOnSelect: true,
    // Terminal right-click menu: the component supplies Copy/Paste/Select-All/
    // Clear; the app appends tab/window/split actions targeting this pane.
    menuItems: () => [
      { label: 'Split Right', accel: '⌘D', action: () => splitActive('row') },
      { label: 'Split Down', accel: '⇧⌘D', action: () => splitActive('col') },
      { label: 'Find…', accel: '⌘F', action: () => openFind() },
      { separator: true },
      { label: 'New Tab', accel: '⌘T', action: () => newTab() },
      { label: 'New Window', accel: '⌘N', action: () => newWindow() },
      { separator: true },
      { label: 'Close Pane', accel: '⌘W', action: () => activeLeaf && closePane(activeLeaf) },
    ],
  });
  const s = { id, term, title: 'shell', spawned: false };
  sessions.set(id, s);
  term.onData((bytes) => invoke('pty_write', { id, data: Array.from(bytes) }));
  term.onResize((cols, rows) => invoke('pty_resize', { id, cols, rows }));
  term.onTitleChange((t) => {
    s.title = t || 'shell';
    refreshTabLabel();
  });
  return s;
}

function makeLeaf(session) {
  const el = document.createElement('div');
  el.className = 'pane';
  const leaf = { kind: 'leaf', session, el };
  el.addEventListener('pointerdown', () => setActiveLeaf(leaf), true);
  return leaf;
}

function setActiveLeaf(leaf) {
  if (activeLeaf === leaf) return;
  activeLeaf = leaf;
  for (const t of tabs) {
    for (const l of leaves(t.root)) l.el.classList.toggle('focused', l === leaf);
  }
  leaf.session.term.focus();
  updateHud();
}

function* leaves(node) {
  if (node.kind === 'leaf') yield node;
  else {
    yield* leaves(node.a);
    yield* leaves(node.b);
  }
}

// --- tabs ------------------------------------------------------------------

function newTab() {
  const session = newSession();
  const root = makeLeaf(session);
  const container = document.createElement('div');
  container.className = 'tab-view';
  stack.appendChild(container);

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

  const tab = { id: session.id, root, container, tabEl, labelEl };
  tabs.push(tab);
  tabEl.addEventListener('mousedown', (e) => {
    if (e.target !== closeEl) activate(tab.id);
  });
  closeEl.addEventListener('click', (e) => {
    e.stopPropagation();
    closeTab(tab.id);
  });

  activate(tab.id);
  return tab;
}

function activate(tabId) {
  if (activeTabId === tabId) return;
  const prev = tabs.find((t) => t.id === activeTabId);
  if (prev) {
    prev.container.classList.remove('active');
    prev.tabEl.classList.remove('active');
    for (const l of leaves(prev.root)) l.session.term.detachView(); // free GPU/CPU
  }
  const tab = tabs.find((t) => t.id === tabId);
  if (!tab) return;
  activeTabId = tabId;
  tab.container.classList.add('active');
  tab.tabEl.classList.add('active');
  renderTree(tab);
  const first = [...leaves(tab.root)][0];
  setActiveLeaf(first);
}

async function closeTab(tabId) {
  const tab = tabs.find((t) => t.id === tabId);
  if (!tab) return;
  for (const l of leaves(tab.root)) await disposeSession(l.session);
  tab.container.remove();
  tab.tabEl.remove();
  tabs.splice(tabs.indexOf(tab), 1);
  if (activeTabId === tabId) {
    activeTabId = null;
    if (tabs.length) activate(tabs[tabs.length - 1].id);
    else newTab();
  }
}

async function disposeSession(session) {
  try {
    await invoke('pty_kill', { id: session.id });
  } catch {
    /* already gone */
  }
  session.term.dispose();
  sessions.delete(session.id);
}

// --- split panes -----------------------------------------------------------

function splitActive(dir) {
  if (!activeLeaf) return;
  const tab = tabs.find((t) => t.id === activeTabId);
  if (!tab) return;
  const oldLeaf = activeLeaf;
  const session = newSession();
  const newLeaf = makeLeaf(session);
  const split = { kind: 'split', dir, a: oldLeaf, b: newLeaf, weightA: 0.5 };

  if (tab.root === oldLeaf) {
    tab.root = split;
  } else {
    const parent = findParent(tab.root, oldLeaf);
    if (parent.a === oldLeaf) parent.a = split;
    else parent.b = split;
  }
  renderTree(tab);
  setActiveLeaf(newLeaf);
}

function closePane(leaf) {
  const tab = tabs.find((t) => [...leaves(t.root)].includes(leaf));
  if (!tab) return;
  if (tab.root === leaf) {
    closeTab(tab.id);
    return;
  }
  disposeSession(leaf.session);
  const parent = findParent(tab.root, leaf);
  const sibling = parent.a === leaf ? parent.b : parent.a;
  const grand = findParent(tab.root, parent);
  if (!grand) tab.root = sibling;
  else if (grand.a === parent) grand.a = sibling;
  else grand.b = sibling;
  renderTree(tab);
  setActiveLeaf([...leaves(tab.root)][0]);
}

function findParent(node, target) {
  if (node.kind !== 'split') return null;
  if (node.a === target || node.b === target) return node;
  return findParent(node.a, target) || findParent(node.b, target);
}

// Rebuild a tab's DOM from its pane tree, then attach + fit every visible leaf.
// Fit + spawn are deferred one frame so the flexbox layout has settled and the
// PTY is created at the real terminal size (not a 2-row sliver at boot).
function renderTree(tab) {
  tab.container.innerHTML = '';
  tab.container.appendChild(buildNode(tab, tab.root));
  for (const l of leaves(tab.root)) {
    if (!l.session.term.attached) l.session.term.attachView(l.el);
  }
  requestAnimationFrame(() => {
    for (const l of leaves(tab.root)) {
      if (!l.session.term.attached) continue;
      l.session.term.fit();
      if (!l.session.spawned) {
        l.session.spawned = true;
        invoke('pty_spawn', {
          id: l.session.id,
          cols: l.session.term.cols,
          rows: l.session.term.rows,
        });
      }
    }
  });
}

function buildNode(tab, node) {
  if (node.kind === 'leaf') return node.el;
  const box = document.createElement('div');
  box.className = 'split ' + (node.dir === 'row' ? 'split-row' : 'split-col');
  const a = buildNode(tab, node.a);
  const b = buildNode(tab, node.b);
  a.style.flex = `${node.weightA}`;
  b.style.flex = `${1 - node.weightA}`;
  const divider = document.createElement('div');
  divider.className = 'divider ' + (node.dir === 'row' ? 'divider-v' : 'divider-h');
  makeDraggable(divider, node, box, tab);
  box.append(a, divider, b);
  return box;
}

function makeDraggable(divider, node, box, tab) {
  divider.addEventListener('mousedown', (e) => {
    e.preventDefault();
    const rect = box.getBoundingClientRect();
    const move = (ev) => {
      const t =
        node.dir === 'row'
          ? (ev.clientX - rect.left) / rect.width
          : (ev.clientY - rect.top) / rect.height;
      node.weightA = Math.max(0.1, Math.min(0.9, t));
      box.children[0].style.flex = `${node.weightA}`;
      box.children[2].style.flex = `${1 - node.weightA}`;
      for (const l of leaves(node)) l.session.term.fit();
    };
    const up = () => {
      window.removeEventListener('mousemove', move);
      window.removeEventListener('mouseup', up);
    };
    window.addEventListener('mousemove', move);
    window.addEventListener('mouseup', up);
  });
}

function findLeafBySession(id) {
  for (const t of tabs) for (const l of leaves(t.root)) if (l.session.id === id) return l;
  return null;
}

function refreshTabLabel() {
  for (const t of tabs) {
    const first = [...leaves(t.root)][0];
    const n = [...leaves(t.root)].length;
    t.labelEl.textContent = first.session.title + (n > 1 ? ` (${n})` : '');
    t.labelEl.title = first.session.title;
  }
}

// --- font zoom / clear / find ----------------------------------------------

function zoom(delta) {
  fontSize = Math.max(6, Math.min(40, fontSize + delta));
  for (const s of sessions.values()) s.term.setFontSize(fontSize);
  refitActive();
}
function zoomReset() {
  fontSize = 13;
  for (const s of sessions.values()) s.term.setFontSize(fontSize);
  refitActive();
}
function refitActive() {
  const tab = tabs.find((t) => t.id === activeTabId);
  if (tab) for (const l of leaves(tab.root)) l.session.term.fit();
}

// Simple find overlay: searches the active pane's scrollback + screen and
// scrolls to each match.
let findState = null;
function openFind() {
  if (findState) {
    findState.input.focus();
    return;
  }
  const term = activeLeaf?.session.term;
  if (!term) return;
  const bar = document.createElement('div');
  bar.className = 'find-bar';
  bar.innerHTML =
    '<input placeholder="Find" /><span class="count"></span>' +
    '<button data-a="prev">↑</button><button data-a="next">↓</button><button data-a="close">✕</button>';
  document.body.appendChild(bar);
  const input = bar.querySelector('input');
  const count = bar.querySelector('.count');
  findState = { bar, input, count, term, matches: [], idx: -1 };
  input.focus();

  const run = () => {
    findState.matches = term.findAll(input.value);
    findState.idx = findState.matches.length ? 0 : -1;
    show();
  };
  const show = () => {
    const m = findState.matches;
    count.textContent = m.length ? `${findState.idx + 1}/${m.length}` : input.value ? '0/0' : '';
    if (findState.idx >= 0) term.scrollToLine(m[findState.idx].line);
  };
  const step = (d) => {
    if (!findState.matches.length) return;
    findState.idx = (findState.idx + d + findState.matches.length) % findState.matches.length;
    show();
  };
  input.addEventListener('input', run);
  input.addEventListener('keydown', (e) => {
    if (e.key === 'Enter') step(e.shiftKey ? -1 : 1);
    else if (e.key === 'Escape') closeFind();
  });
  bar.querySelector('[data-a="next"]').addEventListener('click', () => step(1));
  bar.querySelector('[data-a="prev"]').addEventListener('click', () => step(-1));
  bar.querySelector('[data-a="close"]').addEventListener('click', () => closeFind());
}
function closeFind() {
  if (!findState) return;
  findState.term.scrollToBottom();
  findState.bar.remove();
  findState = null;
  activeLeaf?.session.term.focus();
}

// --- multi-window ----------------------------------------------------------

function newWindow() {
  if (!WebviewWindow) {
    newTab();
    return;
  }
  const label = 'ftwin_' + Math.random().toString(36).slice(2, 8);
  new WebviewWindow(label, {
    url: 'index.html',
    title: '',
    width: 1000,
    height: 660,
    titleBarStyle: 'Overlay',
    backgroundColor: '#14151f',
  });
}

// --- HUD -------------------------------------------------------------------

const hudFps = document.querySelector('#hud-fps b');
const hudTput = document.querySelector('#hud-tput b');
const hudRenderer = document.getElementById('hud-renderer');
const hudBattery = document.getElementById('hud-battery');

function updateHud() {
  hudRenderer.textContent = activeLeaf ? activeLeaf.session.term.rendererName || '–' : '–';
}

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

setInterval(() => {
  hudTput.textContent = Math.round(bytesThisSecond / 1024);
  bytesThisSecond = 0;
}, 1000);

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
    hudBattery.innerHTML = `${b.charging ? '⚡' : ''}<b>${pct}%</b>${
      b.seconds_remaining ? ' · ' + fmtTime(b.seconds_remaining) : ''
    }`;
    hudBattery.classList.toggle('low', pct <= 20 && !b.charging);
  } catch {
    hudBattery.textContent = '';
  }
}
pollBattery();
setInterval(pollBattery, 5000);

// --- keyboard shortcuts ----------------------------------------------------

window.addEventListener('keydown', (e) => {
  const meta = e.metaKey || e.ctrlKey;
  if (!meta) return;
  const k = e.key;
  if (k === 't') return act(e, () => newTab());
  if (k === 'n') return act(e, () => newWindow());
  if (k === 'w') return act(e, () => activeLeaf && closePane(activeLeaf));
  if (k === 'd') return act(e, () => splitActive(e.shiftKey ? 'col' : 'row'));
  if (k === 'f') return act(e, () => openFind());
  if (k === 'k') return act(e, () => activeLeaf?.session.term.clear());
  if (k === '=' || k === '+') return act(e, () => zoom(1));
  if (k === '-') return act(e, () => zoom(-1));
  if (k === '0') return act(e, () => zoomReset());
  if (k >= '1' && k <= '9') {
    const idx = +k - 1;
    if (tabs[idx]) act(e, () => activate(tabs[idx].id));
  }
});
function act(e, fn) {
  e.preventDefault();
  fn();
}

document.getElementById('new-tab').addEventListener('click', () => newTab());

// --- app-level context menus (tab bar, titlebar, new-tab button) -----------

let appMenu = null;
function showMenu(x, y, items) {
  closeAppMenu();
  const menu = document.createElement('div');
  menu.className = 'app-menu';
  for (const item of items) {
    if (item.separator) {
      const s = document.createElement('div');
      s.className = 'sep';
      menu.appendChild(s);
      continue;
    }
    const el = document.createElement('div');
    el.className = 'item' + (item.enabled === false ? ' disabled' : '');
    el.innerHTML = `<span>${item.label}</span>${item.accel ? `<span class="accel">${item.accel}</span>` : ''}`;
    if (item.enabled !== false) {
      el.addEventListener('mousedown', (e) => {
        e.preventDefault();
        e.stopPropagation();
        closeAppMenu();
        item.action();
      });
    }
    menu.appendChild(el);
  }
  document.body.appendChild(menu);
  const r = menu.getBoundingClientRect();
  menu.style.left = Math.min(x, window.innerWidth - r.width - 8) + 'px';
  menu.style.top = Math.min(y, window.innerHeight - r.height - 8) + 'px';
  appMenu = menu;
  setTimeout(() => window.addEventListener('mousedown', closeAppMenu, { once: true }), 0);
}
function closeAppMenu() {
  if (appMenu) {
    appMenu.remove();
    appMenu = null;
  }
}

function tabBarMenu(x, y, tab) {
  const items = [
    { label: 'New Tab', accel: '⌘T', action: () => newTab() },
    { label: 'New Window', accel: '⌘N', action: () => newWindow() },
  ];
  if (tab) {
    items.push(
      { separator: true },
      { label: 'Split Right', accel: '⌘D', action: () => { activate(tab.id); splitActive('row'); } },
      { label: 'Split Down', accel: '⇧⌘D', action: () => { activate(tab.id); splitActive('col'); } },
      { separator: true },
      { label: 'Close Tab', accel: '⌘W', action: () => closeTab(tab.id) }
    );
  }
  showMenu(x, y, items);
}

document.getElementById('titlebar').addEventListener('contextmenu', (e) => {
  e.preventDefault();
  const tabEl = e.target.closest('.tab');
  const tab = tabEl ? tabs.find((t) => t.tabEl === tabEl) : null;
  tabBarMenu(e.clientX, e.clientY, tab);
});
document.getElementById('new-tab').addEventListener('contextmenu', (e) => {
  e.preventDefault();
  tabBarMenu(e.clientX, e.clientY, null);
});

// --- boot ------------------------------------------------------------------

// WASM must be initialized before any `new Ferroterm(...)`.
await Ferroterm.ready();
newTab();
