// Link support: resolves the URI under a grid position, combining OSC 8
// hyperlinks (authoritative, carried per-cell) with automatic detection of
// plain URLs in the row text. Used for hover-underline and click-to-open.

// Reasonably strict URL matcher: http(s)/ftp/file/mailto, no trailing junk.
const URL_RE =
  /\b((?:https?|ftp|file):\/\/[^\s<>"'`]+|mailto:[^\s<>"'`]+|www\.[^\s<>"'`]+)/g;

// Trailing characters that are almost never part of the URL.
const TRAILING = /[.,;:!?)\]}'"]+$/;

/**
 * Given the model and a cell position, return `{ uri, y, x0, x1 }` for the link
 * under the cursor, or `null`. `resolveOsc` maps an OSC 8 id to a URI.
 */
export function linkAt(model, x, y, resolveOsc) {
  if (y < 0 || y >= model.rows || x < 0 || x >= model.cols) return null;
  const i = model.index(x, y);

  // OSC 8 takes precedence.
  const id = model.link[i];
  if (id !== 0) {
    const uri = resolveOsc(id);
    if (uri) {
      // Extend the run over all adjacent cells sharing this id.
      let x0 = x;
      let x1 = x;
      const off = y * model.cols;
      while (x0 > 0 && model.link[off + x0 - 1] === id) x0--;
      while (x1 < model.cols - 1 && model.link[off + x1 + 1] === id) x1++;
      return { uri, y, x0, x1 };
    }
  }

  // Auto-detect a URL in the row text.
  const text = model.rowText(y);
  URL_RE.lastIndex = 0;
  let m;
  while ((m = URL_RE.exec(text)) !== null) {
    let start = m.index;
    let raw = m[0].replace(TRAILING, '');
    let end = start + raw.length - 1; // inclusive
    if (x >= start && x <= end) {
      let uri = raw;
      if (uri.startsWith('www.')) uri = 'https://' + uri;
      return { uri, y, x0: start, x1: end };
    }
  }
  return null;
}
