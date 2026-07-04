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
        run(term, line.trim());
        line = '';
        prompt();
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
    case 'loadtest':
      return loadtest(term, parseInt(args[0], 10) || 2);
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
      '  ' + c('1;33', 'loadtest') + ' [MB]  stream N MB of data and report MB/s',
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
