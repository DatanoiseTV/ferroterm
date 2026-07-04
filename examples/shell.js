// A tiny self-contained "shell" for the browser demo: no backend, no PTY. It
// echoes typed input, does line editing, and runs a handful of built-in
// commands that exercise the emulator (colors, unicode, links, a load test).
//
// This is demo glue, not part of the library. In a real app you would pipe
// `term.onData` to a PTY over a socket and `term.write` its output back.

const ESC = '\x1b';
const c = (n, s) => `${ESC}[${n}m${s}${ESC}[0m`;

export function attachShell(term) {
  let line = '';
  const prompt = () => term.write(`${c('1;32', 'ferroterm')}${c('90', ':')}${c('1;34', '~')}$ `);

  banner(term);
  prompt();

  term.onData((bytes) => {
    const s = new TextDecoder().decode(bytes);
    for (const ch of s) {
      const code = ch.codePointAt(0);
      if (ch === '\r') {
        term.write('\r\n');
        const r = run(term, line.trim());
        line = '';
        if (r instanceof Promise) r.then(() => prompt());
        else prompt();
      } else if (code === 0x7f || code === 0x08) {
        if (line.length) {
          line = line.slice(0, -1);
          term.write('\b \b');
        }
      } else if (code === 0x03) {
        term.write('^C\r\n');
        line = '';
        prompt();
      } else if (code >= 0x20) {
        line += ch;
        term.write(ch); // local echo
      }
    }
  });
}

function banner(term) {
  term.write(
    [
      '',
      c('1;36', '  ferroterm ') + c('90', '— a Rust/WASM terminal emulator core'),
      '',
      '  Type ' + c('1;33', 'help') + ' for commands. Try ' + c('1;33', 'loadtest') +
        ' to benchmark throughput.',
      '',
    ].join('\r\n') + '\r\n'
  );
}

export function runCommand(term, cmd) {
  run(term, cmd);
}

function run(term, cmd) {
  const [name, ...args] = cmd.split(/\s+/);
  switch (name) {
    case '':
      return;
    case 'help':
      return help(term);
    case 'ls':
      return ls(term);
    case 'colors':
      return colors(term);
    case 'chars':
      return chars(term);
    case 'links':
      return links(term);
    case 'sixel':
      return sixel(term);
    case 'loadtest':
      return loadtest(term, parseInt(args[0], 10) || 2);
    case 'bench':
    case 'benchmark':
      return benchmark(term, parseInt(args[0], 10) || 4);
    case 'clear':
      return term.write('\x1b[2J\x1b[H');
    default:
      term.write(c('1;31', `ferroterm: command not found: ${name}`) + '\r\n');
  }
}

function help(term) {
  term.write(
    [
      c('1;37', 'Commands:'),
      '  ' + c('1;33', 'help') + '      show this message',
      '  ' + c('1;33', 'ls') + '        list a fake directory (colors + a link)',
      '  ' + c('1;33', 'colors') + '    print the 256-color palette',
      '  ' + c('1;33', 'chars') + '     print styles & unicode / wide glyphs',
      '  ' + c('1;33', 'links') + '     print clickable hyperlinks (OSC 8 + auto)',
      '  ' + c('1;33', 'sixel') + '     draw a Sixel image (graphics)',
      '  ' + c('1;33', 'loadtest') + ' [MB]  stream N MB and report MB/s',
      '  ' + c('1;33', 'benchmark') + ' [MB] full suite: parse scenarios + renderer paint',
      '  ' + c('1;33', 'clear') + '     clear the screen',
      '',
    ].join('\r\n') + '\r\n'
  );
}

function ls(term) {
  term.write(
    [
      c('1;34', 'src') + '  ' + c('1;34', 'target') + '  ' + c('1;32', 'build.sh') +
        '  README.md  Cargo.toml',
      c('90', '# tip:') + ' the name above is a link -> ' +
        '\x1b]8;;https://github.com/DatanoiseTV/ferroterm\x07' +
        c('4;36', 'github.com/DatanoiseTV/ferroterm') +
        '\x1b]8;;\x07',
      '',
    ].join('\r\n') + '\r\n'
  );
}

function colors(term) {
  let out = '';
  for (let i = 0; i < 256; i++) {
    out += `\x1b[48;5;${i}m ${i.toString().padStart(3)} \x1b[0m`;
    if ((i + 1) % 8 === 0) out += '\r\n';
  }
  term.write(out + '\r\n');
}

function chars(term) {
  term.write(
    [
      c('1', 'bold') + ' ' + c('3', 'italic') + ' ' + c('4', 'underline') + ' ' +
        c('9', 'strike') + ' ' + c('7', 'inverse') + ' ' + c('2', 'dim'),
      c('38;2;255;120;0', 'truecolor orange') + '  ' + c('38;2;0;200;255', 'truecolor cyan'),
      'wide: ' + c('1;35', '你好世界 こんにちは 안녕하세요') + '  emoji: 🦀 🚀 ✨ 🔥',
      'box: ' + c('36', '┌──┬──┐  ├──┼──┤  └──┴──┘  ═══ ║ ▏▎▍▌▋▊▉█'),
      '',
    ].join('\r\n') + '\r\n'
  );
}

function links(term) {
  term.write(
    [
      'OSC 8:  ' + '\x1b]8;;https://www.rust-lang.org\x07' + c('4;33', 'The Rust Programming Language') +
        '\x1b]8;;\x07',
      'Auto:   https://github.com/DatanoiseTV/ferroterm and mailto:hi@example.com',
      c('90', '(hover to underline, click to open)'),
      '',
    ].join('\r\n') + '\r\n'
  );
}

// Draws a Sixel image: a smooth 64x48 HSV gradient plus color bands, to show
// the graphics pipeline (DCS Sixel -> decode -> overlay render).
function sixel(term) {
  const W = 64, H = 48; // pixels; H must be a multiple of 6
  // Build an RGB palette (colors 1..N) and paint per-column sixel data.
  const bands = H / 6;
  let out = '\x1bPq"1;1;' + W + ';' + H;
  // Define 32 palette colors as a hue sweep.
  const N = 32;
  for (let i = 0; i < N; i++) {
    const h = (i / N) * 360;
    const [r, g, b] = hsvToPct(h, 100, 100);
    out += `#${i + 1};2;${r};${g};${b}`;
  }
  for (let band = 0; band < bands; band++) {
    // For each color, emit the columns that use it on this band (all 6 rows on).
    for (let ci = 0; ci < N; ci++) {
      out += `#${ci + 1}`;
      let run = '';
      for (let x = 0; x < W; x++) {
        // color index depends on x (horizontal hue) and band (vertical shade).
        const idx = ((x * N / W) | 0);
        run += idx === ci ? '~' : '?'; // '~'=all six rows, '?'=none
      }
      out += rle(run);
      out += '$'; // graphics CR: overlay next color on the same band
    }
    out += '-'; // next band
  }
  out += '\x1b\\';
  term.write('A Sixel image (64x48):\r\n');
  term.write(out);
  term.write('\r\n');
}

// Run-length encode a sixel row string using the `!Pn` repeat form.
function rle(s) {
  let o = '';
  let i = 0;
  while (i < s.length) {
    let j = i;
    while (j < s.length && s[j] === s[i]) j++;
    const n = j - i;
    o += n >= 4 ? `!${n}${s[i]}` : s.slice(i, j);
    i = j;
  }
  return o;
}

function hsvToPct(h, s, v) {
  s /= 100; v /= 100;
  const c = v * s, x = c * (1 - Math.abs(((h / 60) % 2) - 1)), m = v - c;
  let r = 0, g = 0, b = 0;
  const seg = (h / 60) | 0;
  if (seg === 0) [r, g, b] = [c, x, 0];
  else if (seg === 1) [r, g, b] = [x, c, 0];
  else if (seg === 2) [r, g, b] = [0, c, x];
  else if (seg === 3) [r, g, b] = [0, x, c];
  else if (seg === 4) [r, g, b] = [x, 0, c];
  else [r, g, b] = [c, 0, x];
  return [Math.round((r + m) * 100), Math.round((g + m) * 100), Math.round((b + m) * 100)];
}

// Streams `mb` megabytes of colorful text through the terminal and reports the
// wall-clock throughput, echoing the classic xterm.js loadtest.
function loadtest(term, mb) {
  const target = mb * 1024 * 1024;
  const words = ['ferroterm', 'wasm', 'rust', 'render', 'parser', 'vt100', 'grid', 'scroll'];
  let payload = '';
  while (payload.length < target) {
    let line = '';
    for (let i = 0; i < 10; i++) {
      const col = 31 + ((Math.random() * 7) | 0);
      line += `\x1b[${col}m${words[(Math.random() * words.length) | 0]}\x1b[0m `;
    }
    payload += line + '\r\n';
  }
  const bytes = new TextEncoder().encode(payload);
  const start = performance.now();
  term.write(bytes);
  // Force a synchronous render frame to measure end-to-end cost.
  requestAnimationFrame(() => {
    const ms = performance.now() - start;
    const mbps = (bytes.length / 1e6 / (ms / 1000)).toFixed(1);
    term.write(
      '\r\n' +
        c('1;32', `Wrote ${(bytes.length / 1024).toFixed(0)}kB in ${ms.toFixed(0)}ms`) +
        c('90', ` (${mbps} MB/s, ${term.rendererName} renderer)`) +
        '\r\n'
    );
  });
}

// ---------------------------------------------------------------------------
// Built-in benchmark: parse throughput across representative workloads, plus a
// Canvas2D-vs-WebGL paint comparison. Prints formatted result tables.
// ---------------------------------------------------------------------------

const WORDS = ['ferroterm', 'wasm', 'rust', 'render', 'parser', 'vt100', 'grid', 'scroll', 'buffer', 'atlas'];
// A tiny deterministic PRNG so runs are comparable.
function rng(seed) {
  let s = seed >>> 0;
  return () => {
    s ^= s << 13; s ^= s >>> 17; s ^= s << 5;
    return (s >>> 0) / 0xffffffff;
  };
}

function genPlain(target) {
  const r = rng(1);
  let out = '';
  while (out.length < target) {
    let line = '';
    for (let i = 0; i < 12; i++) line += WORDS[(r() * WORDS.length) | 0] + ' ';
    out += line + '\r\n';
  }
  return out;
}
function genSgr(target) {
  const r = rng(2);
  let out = '';
  while (out.length < target) {
    let line = '';
    for (let i = 0; i < 10; i++) line += `\x1b[38;5;${(r() * 256) | 0}m` + WORDS[(r() * WORDS.length) | 0] + ' ';
    out += line + '\x1b[0m\r\n';
  }
  return out;
}
function genTrue(target) {
  const r = rng(3);
  let out = '';
  while (out.length < target) {
    let line = '';
    for (let i = 0; i < 8; i++)
      line += `\x1b[38;2;${(r() * 256) | 0};${(r() * 256) | 0};${(r() * 256) | 0}m` + WORDS[(r() * WORDS.length) | 0] + ' ';
    out += line + '\x1b[0m\r\n';
  }
  return out;
}
function genCursor(target) {
  // Progress-bar style: home, redraw a line with cursor moves + erase.
  const r = rng(4);
  let out = '';
  while (out.length < target) {
    const pct = (r() * 100) | 0;
    out += `\x1b[1;1H\x1b[2K[${'#'.repeat(pct / 4 | 0)}${'-'.repeat(25 - (pct / 4 | 0))}] ${pct}% `;
    out += `\x1b[2;1H\x1b[2Kframe ${(r() * 100000) | 0}`;
  }
  return out;
}
function genScroll(target, cols) {
  const r = rng(5);
  let out = '';
  const w = Math.max(40, cols);
  while (out.length < target) {
    let line = '';
    while (line.length < w) line += WORDS[(r() * WORDS.length) | 0] + ' ';
    out += line.slice(0, w) + '\r\n';
  }
  return out;
}

function benchmark(term, mb) {
  const target = mb * 1024 * 1024;
  const scenarios = [
    ['plain text', genPlain(target), '1;37'],
    ['256-color (SGR)', genSgr(target), '1;36'],
    ['true color', genTrue(target), '1;35'],
    ['cursor / progress', genCursor(target), '1;33'],
    ['scroll (full-width)', genScroll(target, term.cols), '1;32'],
  ];

  const enc = new TextEncoder();
  term.write(c('1;96', 'running benchmark…') + c('90', ` (${mb} MB per scenario)`) + '\r\n');
  const results = scenarios.map(([name, payload, col]) => {
    const bytes = enc.encode(payload);
    term.write('\x1b[2J\x1b[H');
    const t0 = performance.now();
    term.write(bytes); // synchronous parse — this is the emulator's core cost
    const ms = performance.now() - t0;
    return { name, col, mb: bytes.length / 1e6, ms, mbps: bytes.length / 1e6 / (ms / 1000) };
  });

  // Renderer paint comparison (async), then print everything together so the
  // fullscreen paint frames don't overwrite the result tables.
  return measureRenderers(term).then((paint) => {
    term.write('\x1b[2J\x1b[H');
    printParseTable(term, results, mb);
    printPaintTable(term, paint, term.cols, term.rows);
  });
}

const nextFrame = () => new Promise((r) => requestAnimationFrame(r));

function fullScreenFrame(term, frame) {
  let s = '\x1b[H';
  for (let y = 0; y < term.rows; y++) {
    let line = '';
    for (let x = 0; x < term.cols; x++) {
      const cc = (x + y + frame) % 256;
      line += `\x1b[48;5;${cc}m\x1b[38;5;${(cc + 128) % 256}m*`;
    }
    s += line + '\x1b[0m';
    if (y < term.rows - 1) s += '\r\n';
  }
  return s;
}

async function measureRenderers(term) {
  const original = term.rendererName?.toLowerCase().includes('canvas') ? 'canvas' : 'webgl';
  const out = [];
  for (const kind of ['canvas', 'webgl']) {
    term.setRenderer(kind);
    await nextFrame();
    // warm up (glyph atlas / first paint)
    term.write(fullScreenFrame(term, 0));
    await nextFrame();
    const frames = 24;
    let total = 0;
    let best = Infinity;
    for (let f = 1; f <= frames; f++) {
      const t0 = performance.now();
      term.write(fullScreenFrame(term, f));
      await nextFrame();
      const dt = performance.now() - t0;
      total += dt;
      best = Math.min(best, dt);
    }
    out.push({ kind: term.rendererName, avg: total / frames, best });
  }
  term.setRenderer(original);
  await nextFrame();
  term.write('\x1b[2J\x1b[H');
  return out;
}

// --- table formatting ---

const strip = (s) => s.replace(/\x1b\[[0-9;]*m/g, '');
const padL = (s, n) => ' '.repeat(Math.max(0, n - strip(s).length)) + s;
const padR = (s, n) => s + ' '.repeat(Math.max(0, n - strip(s).length));
const B = (s) => c('90', s); // border color

function drawTable(term, title, cols, rows) {
  const widths = cols.map((col, i) =>
    Math.max(strip(col.head).length, ...rows.map((r) => strip(r[i]).length))
  );
  const line = (l, m, rr) =>
    B(l + widths.map((w) => '─'.repeat(w + 2)).join(m) + rr);
  const fmt = (cells) =>
    B('│') +
    cells
      .map((cell, i) => ' ' + (cols[i].align === 'r' ? padL(cell, widths[i]) : padR(cell, widths[i])) + ' ')
      .join(B('│')) +
    B('│');

  const totalW = widths.reduce((a, w) => a + w + 3, 1);
  const pad = Math.max(0, totalW - 2 - strip(title).length);
  term.write(B('╭─') + title + B('─'.repeat(pad) + '╮') + '\r\n');
  term.write(fmt(cols.map((col) => c('1;97', col.head))) + '\r\n');
  term.write(line('├', '┼', '┤') + '\r\n');
  for (const r of rows) term.write(fmt(r) + '\r\n');
  term.write(line('╰', '┴', '╯') + '\r\n');
}

function printParseTable(term, results, mb) {
  const bar = (mbps) => {
    const max = 400; // MB/s full bar
    const n = Math.min(16, Math.round((mbps / max) * 16));
    return c('32', '█'.repeat(n)) + c('90', '░'.repeat(16 - n));
  };
  const rows = results.map((r) => [
    c(r.col, r.name),
    `${r.mb.toFixed(1)} MB`,
    `${r.ms.toFixed(0)} ms`,
    c('1;92', `${r.mbps.toFixed(0)} MB/s`),
    bar(r.mbps),
  ]);
  const avg = results.reduce((a, r) => a + r.mbps, 0) / results.length;
  drawTable(
    term,
    c('1;96', ` ferroterm parse benchmark `) + c('90', `(${mb} MB each, ${term.rendererName}) `),
    [
      { head: 'scenario', align: 'l' },
      { head: 'data', align: 'r' },
      { head: 'time', align: 'r' },
      { head: 'throughput', align: 'r' },
      { head: 'rate', align: 'l' },
    ],
    rows
  );
  term.write(
    c('90', '  average parse throughput: ') + c('1;92', `${avg.toFixed(0)} MB/s`) + '\r\n\r\n'
  );
}

function printPaintTable(term, paint, cols, rows) {
  const trows = paint.map((p) => [
    c('1;37', p.kind),
    `${p.avg.toFixed(2)} ms`,
    `${p.best.toFixed(2)} ms`,
    c('1;92', `${(1000 / p.avg).toFixed(0)} fps`),
  ]);
  drawTable(
    term,
    c('1;96', ` renderer paint `) + c('90', `(full ${cols}x${rows} redraw/frame) `),
    [
      { head: 'renderer', align: 'l' },
      { head: 'avg frame', align: 'r' },
      { head: 'best frame', align: 'r' },
      { head: 'fps', align: 'r' },
    ],
    trows
  );
  term.write(c('90', '  (lower frame time is better; both redraw every cell each frame)') + '\r\n');
}
