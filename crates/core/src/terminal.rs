//! The terminal state machine: implements [`Perform`] to turn parser tokens
//! into buffer mutations, tracks modes, scrollback and the alt-screen, resolves
//! OSC 8 hyperlinks, and produces render snapshots.

use std::collections::VecDeque;

use crate::cell::{attr, Cell, Color, Pen};
use crate::grid::{Buffer, Line, SavedCursor};
use crate::palette;
use crate::parser::{Params, Parser, Perform};
use crate::width::char_width;

/// Which dynamic default color an OSC 10/11/12 sequence targets.
#[derive(Clone, Copy)]
enum DynColor {
    Fg,
    Bg,
    Cursor,
}

/// The render-snapshot magic word (little-endian `F3E7` + version).
pub const SNAPSHOT_MAGIC: u32 = 0xF3E7_0001;
/// Words per cell in a snapshot: `[codepoint, fg, bg, flags, link]`.
pub const SNAPSHOT_CELL_WORDS: usize = 6;

/// DEC private + ANSI modes we track. Exposed to the host so it can encode
/// keyboard and mouse input the way the running program expects.
#[derive(Clone, Copy, Debug)]
pub struct Modes {
    pub autowrap: bool,        // DECAWM (7)
    pub cursor_visible: bool,  // DECTCEM (25)
    pub cursor_blink: bool,    // (12)
    pub app_cursor_keys: bool, // DECCKM (1)
    pub app_keypad: bool,      // DECKPAM
    pub insert: bool,          // IRM (4)
    pub newline_mode: bool,    // LNM (20)
    pub bracketed_paste: bool, // (2004)
    pub focus_events: bool,    // (1004)
    pub origin: bool,          // DECOM (6)
    /// Mouse tracking: 0=off, 9=X10, 1000=button, 1002=drag, 1003=any.
    pub mouse_mode: u16,
    /// Mouse encoding: 0=default, 1006=SGR.
    pub mouse_sgr: bool,
    pub reverse_video: bool, // DECSCNM (5)
}

impl Default for Modes {
    fn default() -> Self {
        Modes {
            autowrap: true,
            cursor_visible: true,
            cursor_blink: true,
            app_cursor_keys: false,
            app_keypad: false,
            insert: false,
            newline_mode: false,
            bracketed_paste: false,
            focus_events: false,
            origin: false,
            mouse_mode: 0,
            mouse_sgr: false,
            reverse_video: false,
        }
    }
}

/// A full terminal emulator instance.
pub struct Terminal {
    parser: Parser,

    primary: Buffer,
    alt: Buffer,
    alt_active: bool,

    /// Scrollback ring for the primary screen (oldest at front).
    scrollback: VecDeque<Line>,
    max_scrollback: usize,
    /// Lines scrolled up into history for viewing (0 = at bottom).
    display_offset: usize,
    /// Set when the viewport must be fully re-emitted (offset change / resize).
    viewport_full: bool,

    pen: Pen,
    cur_link: u32,

    modes: Modes,

    /// OSC 8 hyperlink URIs; `link` id `n` -> `links[n - 1]`.
    links: Vec<String>,

    /// Grapheme-cluster strings; cell `grapheme` id `n` -> `graphemes[n - 1]`.
    graphemes: Vec<String>,
    grapheme_ids: std::collections::HashMap<String, u32>,
    /// Position of the last printed base cell, for attaching combining marks.
    last_grapheme: Option<(usize, usize)>,
    /// A ZWJ (U+200D) was just seen; the next scalar joins the current cluster.
    pending_zwj: bool,
    /// The last printed base was a lone regional indicator (for flag pairing).
    last_ri: bool,

    title: String,
    title_dirty: bool,

    /// Bytes the terminal wants to send back to the host (DSR/DA replies etc.).
    output: Vec<u8>,
    /// BEL counter — host can flash / beep.
    pub bell_count: u32,

    /// Dynamic palette state set via OSC 4/10/11/12. `None` = use the theme /
    /// standard xterm value. Indexed overrides are the 256 palette entries;
    /// fg/bg/cursor are the dynamic default colors.
    palette_indexed: Vec<Option<(u8, u8, u8)>>,
    palette_fg: Option<(u8, u8, u8)>,
    palette_bg: Option<(u8, u8, u8)>,
    palette_cursor: Option<(u8, u8, u8)>,
    /// Theme defaults the front-end configured, used only to answer OSC color
    /// *queries* (`?`) for colors the program hasn't overridden.
    default_fg: (u8, u8, u8),
    default_bg: (u8, u8, u8),
    default_cursor: (u8, u8, u8),
    /// Bumped on every palette change so the front-end knows to re-read.
    palette_version: u32,

    /// Decoded Sixel images, anchored in absolute line-serial space so they
    /// scroll with the text they were drawn under.
    images: Vec<ImageRec>,
    next_image_id: u32,
    /// Bumped whenever the image set changes (added / pruned), so the front-end
    /// knows to re-sync its texture cache.
    images_version: u32,
    /// Total number of lines that have scrolled off the top of the primary
    /// screen over the terminal's lifetime. The current top visible primary row
    /// has this serial; an image's fixed serial minus this gives its row.
    scrolled_off: i64,
    /// Cell size in device pixels, set by the front-end. Sixel images are laid
    /// out and advance the cursor in whole cells using this.
    cell_px_w: usize,
    cell_px_h: usize,

    /// Kitty graphics: base64 accumulated across a chunked transmission, paired
    /// with the first chunk's control block (which carries format/action).
    kitty_pending: Option<(crate::kitty::Cmd, Vec<u8>)>,
    /// Kitty images transmitted but not displayed, awaiting a later `a=p`.
    kitty_store: Vec<KittyImage>,
}

/// A placed inline image (Sixel, or an iTerm2 OSC 1337 `File=` image).
struct ImageRec {
    id: u32,
    /// Absolute line serial of the image's top row (see `scrolled_off`).
    serial: i64,
    /// Left column (cell) of the image.
    col: usize,
    /// Display box in device pixels. For Sixel this is the image's native size
    /// (drawn 1:1); for an encoded image it is the cell box the front-end scales
    /// the decoded bitmap into.
    width: usize,
    height: usize,
    /// Height in whole cells (for scroll/prune math).
    rows_cells: usize,
    /// Decoded RGBA pixels (Sixel path); empty for an encoded image.
    rgba: Vec<u8>,
    /// Raw image-file bytes (iTerm2 path); empty for Sixel. The front-end
    /// decodes these natively (`createImageBitmap`) — no decoder in the core.
    encoded: Vec<u8>,
    /// MIME hint for `encoded`; empty for the Sixel/RGBA path.
    mime: &'static str,
    /// True for a Kitty-protocol placement (so `a=d` can target only Kitty
    /// images and leave Sixel / iTerm2 ones alone).
    from_kitty: bool,
    /// Kitty client image id this placement came from (0 = none), so a Kitty
    /// `a=d,d=i` delete can target one specific image.
    kitty_id: u32,
}

/// A Kitty image transmitted but not (yet) displayed, kept so a later `a=p` can
/// place it by id. `data` is already normalized: RGBA pixels for a raw format,
/// or the original PNG file for `f=100`.
#[derive(Clone)]
struct KittyImage {
    id: u32,
    /// Normalized format: 32 (RGBA in `data`) or 100 (PNG in `data`).
    format: u32,
    /// Native pixel size (0 if unknown, e.g. an un-sniffable PNG).
    width: u32,
    height: u32,
    data: Vec<u8>,
}

/// Cap on the base64 accepted for a single (possibly chunked) Kitty image.
const MAX_KITTY_B64: usize = 16 * 1024 * 1024;
/// Cap on total bytes held in the transmit store (images awaiting `a=p`).
const MAX_KITTY_STORE: usize = 32 * 1024 * 1024;

impl Terminal {
    pub fn new(cols: usize, rows: usize, max_scrollback: usize) -> Self {
        Terminal {
            parser: Parser::new(),
            primary: Buffer::new(cols, rows),
            alt: Buffer::new(cols, rows),
            alt_active: false,
            scrollback: VecDeque::new(),
            max_scrollback,
            display_offset: 0,
            viewport_full: true,
            pen: Pen::default(),
            cur_link: 0,
            modes: Modes::default(),
            links: Vec::new(),
            graphemes: Vec::new(),
            grapheme_ids: std::collections::HashMap::new(),
            last_grapheme: None,
            pending_zwj: false,
            last_ri: false,
            title: String::new(),
            title_dirty: false,
            output: Vec::new(),
            bell_count: 0,
            palette_indexed: vec![None; 256],
            palette_fg: None,
            palette_bg: None,
            palette_cursor: None,
            // Match the front-end's DEFAULT_THEME so `?` queries are sensible
            // before the host calls set_default_colors.
            default_fg: (0xe6, 0xe6, 0xe6),
            default_bg: (0x1a, 0x1b, 0x26),
            default_cursor: (0xe6, 0xe6, 0xe6),
            palette_version: 0,
            images: Vec::new(),
            next_image_id: 1,
            images_version: 0,
            scrolled_off: 0,
            cell_px_w: 8,
            cell_px_h: 16,
            kitty_pending: None,
            kitty_store: Vec::new(),
        }
    }

    #[inline]
    pub fn cols(&self) -> usize {
        self.buf().cols
    }
    #[inline]
    pub fn rows(&self) -> usize {
        self.buf().rows
    }
    #[inline]
    pub fn modes(&self) -> &Modes {
        &self.modes
    }
    #[inline]
    pub fn title(&self) -> &str {
        &self.title
    }
    pub fn take_title_dirty(&mut self) -> bool {
        std::mem::take(&mut self.title_dirty)
    }
    #[inline]
    pub fn scrollback_len(&self) -> usize {
        self.scrollback.len()
    }
    #[inline]
    pub fn display_offset(&self) -> usize {
        self.display_offset
    }

    /// The active buffer's line `y` (visible grid, ignores scrollback offset).
    pub fn active_line(&self, y: usize) -> &Line {
        self.buf().line(y.min(self.rows() - 1))
    }

    /// The cursor position of the active buffer.
    pub fn cursor(&self) -> (usize, usize) {
        let c = self.buf().cursor;
        (c.x, c.y)
    }

    // --- text access (search / scraping) -----------------------------------

    /// Total number of logical lines: scrollback history plus the screen.
    pub fn total_lines(&self) -> usize {
        self.scrollback.len() + self.rows()
    }

    /// A borrow of logical line `abs` (0 = oldest scrollback line), clamped into
    /// the active screen for indices past the end.
    fn line_at(&self, abs: usize) -> &Line {
        let back = self.scrollback.len();
        if abs < back {
            &self.scrollback[abs]
        } else {
            self.buf().line((abs - back).min(self.rows() - 1))
        }
    }

    /// Text of logical line `abs` (0 = oldest scrollback line). Wide-glyph
    /// spacer cells are skipped so the string reads naturally.
    pub fn line_text(&self, abs: usize) -> String {
        self.line_at(abs)
            .iter()
            .filter(|c| !c.pen.has(attr::WIDE_SPACER))
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    /// Extract the text of a flow selection given absolute `(col, line)`
    /// endpoints (line 0 = oldest scrollback). The endpoints are normalized to
    /// reading order; the first line is taken from its start column, the last up
    /// to its end column, and whole lines are taken in between. Wide-glyph spacer
    /// cells are skipped, trailing blanks are trimmed per line, and lines are
    /// joined with `\n`. Because it reads the scrollback buffer directly, the
    /// selection may span history rather than just the visible screen.
    pub fn selection_text(&self, a: (usize, usize), b: (usize, usize)) -> String {
        // Normalize so `s` is at or before `e` in reading (row-major) order.
        let (s, e) = if (a.1, a.0) <= (b.1, b.0) {
            (a, b)
        } else {
            (b, a)
        };
        let (sx, sy) = s;
        let (ex, ey) = e;
        let last = self.total_lines().saturating_sub(1);
        let mut lines = Vec::new();
        for abs in sy..=ey.min(last) {
            let x0 = if abs == sy { sx } else { 0 };
            let x1 = if abs == ey { ex } else { usize::MAX };
            let line = self.line_at(abs);
            let hi = x1.min(line.len().saturating_sub(1));
            let mut out = String::new();
            let mut x = x0;
            while x <= hi {
                let cell = &line[x];
                if !cell.pen.has(attr::WIDE_SPACER) {
                    out.push(cell.ch);
                }
                x += 1;
            }
            while out.ends_with(' ') {
                out.pop();
            }
            lines.push(out);
        }
        lines.join("\n")
    }

    /// Scroll the viewport so logical line `abs` is at the top.
    pub fn scroll_to_line(&mut self, abs: usize) {
        let back = self.scrollback.len();
        let off = back.saturating_sub(abs).min(back);
        if off != self.display_offset {
            self.display_offset = off;
            self.viewport_full = true;
        }
    }

    /// Resolve an OSC 8 link id to its URI.
    pub fn link_uri(&self, id: u32) -> Option<&str> {
        if id == 0 {
            None
        } else {
            self.links.get((id - 1) as usize).map(|s| s.as_str())
        }
    }

    /// Feed raw bytes from the host / PTY.
    pub fn feed(&mut self, bytes: &[u8]) {
        // Any output snaps the viewport back to the bottom, matching every
        // real terminal.
        if self.display_offset != 0 {
            self.display_offset = 0;
            self.viewport_full = true;
        }
        // Split borrow: move parser out, run, move back.
        let mut parser = std::mem::take(&mut self.parser);
        parser.advance(self, bytes);
        self.parser = parser;
    }

    /// Drain queued replies destined for the host (send these to the PTY).
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }

    // --- scrollback viewing -------------------------------------------------

    /// Scroll the viewport up (into history) by `n` lines.
    pub fn scroll_up_view(&mut self, n: usize) {
        let max = self.scrollback.len();
        let new = (self.display_offset + n).min(max);
        if new != self.display_offset {
            self.display_offset = new;
            self.viewport_full = true;
        }
    }

    /// Scroll the viewport down (toward the present) by `n` lines.
    pub fn scroll_down_view(&mut self, n: usize) {
        let new = self.display_offset.saturating_sub(n);
        if new != self.display_offset {
            self.display_offset = new;
            self.viewport_full = true;
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        if self.display_offset != 0 {
            self.display_offset = 0;
            self.viewport_full = true;
        }
    }

    // --- resize -------------------------------------------------------------

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols() && rows == self.rows() {
            return;
        }
        // The alternate screen is not reflowed: full-screen apps that use it
        // (editors, pagers) repaint themselves on SIGWINCH, and rewrapping their
        // absolutely-positioned layout would corrupt it. It just resizes.
        self.alt.resize(cols, rows);
        // The primary screen and its scrollback are one continuous stream of
        // logical lines; rewrap them so wrapped text stays intact.
        self.reflow_primary(cols, rows);
        self.display_offset = 0;
        self.viewport_full = true;
    }

    /// Rewrap the primary buffer + scrollback to `new_cols`/`new_rows`, moving
    /// lines between the screen and scrollback as the geometry changes and
    /// keeping the cursor on the same character. See [`crate::reflow`].
    fn reflow_primary(&mut self, new_cols: usize, new_rows: usize) {
        let old_rows = self.primary.rows;
        let cursor = self.primary.cursor;

        // Last row with real content (non-default cell or a soft-wrap flag);
        // trailing blank screen rows below this and the cursor are regenerated
        // rather than pushed through the stream.
        let mut last_content = 0;
        for y in 0..old_rows {
            let line = self.primary.line(y);
            if line.wrapped || line.iter().any(|c| *c != Cell::default()) {
                last_content = y;
            }
        }
        let content_bottom = cursor.y.min(old_rows - 1).max(last_content);

        // Build the physical stream: scrollback (drained) then screen rows.
        let mut phys: Vec<Line> = Vec::with_capacity(self.scrollback.len() + content_bottom + 1);
        phys.extend(self.scrollback.drain(..));
        let sb_before = phys.len();
        for y in 0..=content_bottom {
            phys.push(self.primary.line(y).clone());
        }
        let cursor_row = sb_before + cursor.y.min(content_bottom);
        let cursor_col = cursor.x;

        let r = crate::reflow::reflow(&phys, cursor_row, cursor_col, new_cols);

        // Split the rewrapped stream into scrollback + the visible screen,
        // keeping the cursor on screen.
        let total = r.rows.len();
        let (sb_rows, grid_rows, cy) = if total <= new_rows {
            (Vec::new(), r.rows, r.cursor_row.min(new_rows - 1))
        } else {
            let split = total - new_rows;
            let mut rows = r.rows;
            let grid = rows.split_off(split);
            let cy = r.cursor_row.saturating_sub(split);
            (rows, grid, cy)
        };

        self.scrollback.clear();
        for l in sb_rows {
            self.scrollback.push_back(l);
        }
        while self.scrollback.len() > self.max_scrollback {
            self.scrollback.pop_front();
        }
        self.primary
            .set_grid(new_cols, new_rows, grid_rows, r.cursor_col, cy);
    }

    // --- internal buffer access --------------------------------------------

    #[inline]
    fn buf(&self) -> &Buffer {
        if self.alt_active {
            &self.alt
        } else {
            &self.primary
        }
    }
    #[inline]
    fn buf_mut(&mut self) -> &mut Buffer {
        if self.alt_active {
            &mut self.alt
        } else {
            &mut self.primary
        }
    }

    // --- printing -----------------------------------------------------------

    fn write_char(&mut self, c: char) {
        let w = char_width(c);

        // Zero-width scalars (combining marks, variation selectors, ZWJ) attach
        // to the previous cell's grapheme cluster instead of taking a cell.
        if w == 0 {
            if c == '\u{200D}' {
                // ZWJ: the next scalar joins this cluster.
                self.attach_combining(c);
                self.pending_zwj = true;
            } else {
                self.attach_combining(c);
            }
            return;
        }

        // After a ZWJ, this scalar joins the previous cluster (emoji sequences
        // like family / profession emoji) rather than starting a new cell.
        if self.pending_zwj {
            self.pending_zwj = false;
            self.attach_combining(c);
            return;
        }

        // Regional-indicator pairing: two consecutive RIs form one flag.
        if is_regional_indicator(c) {
            if self.last_ri {
                self.last_ri = false;
                self.pair_regional_indicator(c);
                return;
            }
            self.last_ri = true;
        } else {
            self.last_ri = false;
        }

        let cols = self.cols();
        let autowrap = self.modes.autowrap;

        if self.buf().cursor.pending_wrap && autowrap {
            self.buf_mut().cursor.pending_wrap = false;
            let y = self.buf().cursor.y;
            self.buf_mut().mark_wrapped(y); // the row we're leaving soft-wrapped
            self.carriage_return();
            self.linefeed();
        }

        // A wide glyph that won't fit in the last column wraps first.
        if w == 2 && self.buf().cursor.x == cols - 1 {
            if autowrap {
                let y = self.buf().cursor.y;
                self.buf_mut().mark_wrapped(y);
                self.carriage_return();
                self.linefeed();
            } else {
                self.buf_mut().cursor.x = cols - 2;
            }
        }

        if self.modes.insert {
            let pen = self.pen;
            self.buf_mut().insert_chars(w as usize, pen);
        }

        let (x, y) = {
            let cur = self.buf().cursor;
            (cur.x, cur.y)
        };
        let pen = self.pen;
        let link = self.cur_link;

        {
            let buf = self.buf_mut();
            let mut lead = pen;
            if w == 2 {
                lead.set(attr::WIDE);
            }
            buf.line_mut(y)[x] = Cell {
                ch: c,
                pen: lead,
                link,
                grapheme: 0,
            };
            if w == 2 && x + 1 < cols {
                let mut spacer = pen;
                spacer.set(attr::WIDE_SPACER);
                buf.line_mut(y)[x + 1] = Cell {
                    ch: ' ',
                    pen: spacer,
                    link,
                    grapheme: 0,
                };
            }
        }
        self.last_grapheme = Some((x, y));

        let new_x = x + w as usize;
        let buf = self.buf_mut();
        if new_x >= cols {
            buf.cursor.x = cols - 1;
            buf.cursor.pending_wrap = autowrap;
        } else {
            buf.cursor.x = new_x;
        }
    }

    // --- grapheme clustering ------------------------------------------------

    /// Intern a cluster string, returning its id (deduplicated).
    fn intern_grapheme(&mut self, s: String) -> u32 {
        if let Some(&id) = self.grapheme_ids.get(&s) {
            return id;
        }
        self.graphemes.push(s.clone());
        let id = self.graphemes.len() as u32;
        self.grapheme_ids.insert(s, id);
        id
    }

    fn cluster_of(&self, cell: &Cell) -> String {
        if cell.grapheme == 0 {
            cell.ch.to_string()
        } else {
            self.graphemes
                .get((cell.grapheme - 1) as usize)
                .cloned()
                .unwrap_or_else(|| cell.ch.to_string())
        }
    }

    /// Append a scalar to the last printed cell's cluster.
    fn attach_combining(&mut self, c: char) {
        let Some((x, y)) = self.last_grapheme else {
            return;
        };
        let cell = *self.buf().cell(x, y);
        let mut s = self.cluster_of(&cell);
        // Bound cluster length so hostile input can't grow one cell unboundedly.
        if s.chars().count() >= 16 {
            return;
        }
        s.push(c);
        let id = self.intern_grapheme(s);
        let new = Cell {
            grapheme: id,
            ..cell
        };
        self.buf_mut().line_mut(y)[x] = new;
    }

    /// The second regional indicator of a flag: merge into the previous cell and
    /// widen it to two columns.
    fn pair_regional_indicator(&mut self, c: char) {
        let Some((x, y)) = self.last_grapheme else {
            self.write_char_forced(c);
            return;
        };
        let cols = self.cols();
        let cell = *self.buf().cell(x, y);
        let mut s = self.cluster_of(&cell);
        s.push(c);
        let id = self.intern_grapheme(s);
        let mut pen = cell.pen;
        pen.set(attr::WIDE);
        let base = Cell {
            grapheme: id,
            pen,
            ..cell
        };
        {
            let buf = self.buf_mut();
            buf.line_mut(y)[x] = base;
            if x + 1 < cols {
                let mut spacer = base.pen;
                spacer.clear(attr::WIDE);
                spacer.set(attr::WIDE_SPACER);
                buf.line_mut(y)[x + 1] = Cell {
                    ch: ' ',
                    pen: spacer,
                    link: base.link,
                    grapheme: 0,
                };
            }
        }
        // The first RI occupied one column; consume the second column too.
        let nx = (x + 2).min(cols - 1);
        self.buf_mut().cursor.x = nx;
        if x + 2 >= cols {
            self.buf_mut().cursor.pending_wrap = self.modes.autowrap;
        }
    }

    /// Print `c` as a fresh cell, bypassing cluster/RI state (used as a
    /// fallback). Clears grapheme state first.
    fn write_char_forced(&mut self, c: char) {
        self.last_grapheme = None;
        self.pending_zwj = false;
        self.write_char(c);
    }

    /// Reset grapheme-continuation state; called on any cursor discontinuity so
    /// a later combining mark can't attach across it.
    fn break_grapheme(&mut self) {
        self.last_grapheme = None;
        self.pending_zwj = false;
        self.last_ri = false;
    }

    // --- cursor motion & scrolling -----------------------------------------

    fn carriage_return(&mut self) {
        self.buf_mut().goto_col(0);
    }

    /// Line feed / index: move down one row, scrolling the region if at the
    /// bottom. Feeds scrollback when the primary screen scrolls at row 0.
    fn linefeed(&mut self) {
        let bottom = self.buf().scroll_bottom;
        if self.buf().cursor.y == bottom {
            self.scroll_region_up(1);
        } else {
            let ny = self.buf().cursor.y + 1;
            self.buf_mut().goto_row(ny);
        }
    }

    /// Reverse index: move up one row, scrolling down if at the top.
    fn reverse_index(&mut self) {
        let top = self.buf().scroll_top;
        if self.buf().cursor.y == top {
            let pen = self.pen;
            self.buf_mut().scroll_down(1, pen);
        } else {
            let ny = self.buf().cursor.y - 1;
            self.buf_mut().goto_row(ny);
        }
    }

    fn scroll_region_up(&mut self, n: usize) {
        let pen = self.pen;
        let evict = !self.alt_active && self.buf().scroll_top == 0;
        if evict {
            let mut evicted: Vec<Line> = Vec::new();
            self.primary.scroll_up(n, pen, Some(&mut evicted));
            let count = evicted.len();
            for line in evicted {
                self.scrollback.push_back(line);
                while self.scrollback.len() > self.max_scrollback {
                    self.scrollback.pop_front();
                }
            }
            // The primary screen scrolled: advance the absolute line serial and
            // drop images that have fallen out of retained history.
            self.scrolled_off += count as i64;
            self.prune_images();
        } else {
            self.buf_mut().scroll_up(n, pen, None);
        }
    }

    // --- CSI handlers -------------------------------------------------------

    fn sgr(&mut self, params: &Params) {
        if params.is_empty() {
            self.pen = Pen::default();
            return;
        }
        let mut i = 0;
        let n = params.len();
        while i < n {
            let code = params.get(i, 0);
            match code {
                0 => self.pen = Pen::default(),
                1 => self.pen.set(attr::BOLD),
                2 => self.pen.set(attr::DIM),
                3 => self.pen.set(attr::ITALIC),
                4 => self.pen.set(attr::UNDERLINE),
                5 | 6 => self.pen.set(attr::BLINK),
                7 => self.pen.set(attr::INVERSE),
                8 => self.pen.set(attr::INVISIBLE),
                9 => self.pen.set(attr::STRIKETHROUGH),
                21 | 22 => {
                    self.pen.clear(attr::BOLD);
                    self.pen.clear(attr::DIM);
                }
                23 => self.pen.clear(attr::ITALIC),
                24 => self.pen.clear(attr::UNDERLINE),
                25 => self.pen.clear(attr::BLINK),
                27 => self.pen.clear(attr::INVERSE),
                28 => self.pen.clear(attr::INVISIBLE),
                29 => self.pen.clear(attr::STRIKETHROUGH),
                30..=37 => self.pen.fg = Color::Indexed((code - 30) as u8),
                38 => {
                    if let Some((c, adv)) = parse_ext_color(params, i) {
                        self.pen.fg = c;
                        i += adv;
                    }
                }
                39 => self.pen.fg = Color::Default,
                40..=47 => self.pen.bg = Color::Indexed((code - 40) as u8),
                48 => {
                    if let Some((c, adv)) = parse_ext_color(params, i) {
                        self.pen.bg = c;
                        i += adv;
                    }
                }
                49 => self.pen.bg = Color::Default,
                90..=97 => self.pen.fg = Color::Indexed((code - 90 + 8) as u8),
                100..=107 => self.pen.bg = Color::Indexed((code - 100 + 8) as u8),
                _ => {}
            }
            i += 1;
        }
    }

    fn set_mode(&mut self, params: &Params, private: bool, enable: bool) {
        for p in params.iter() {
            if private {
                self.set_private_mode(p, enable);
            } else {
                match p {
                    4 => self.modes.insert = enable,
                    20 => self.modes.newline_mode = enable,
                    _ => {}
                }
            }
        }
    }

    fn set_private_mode(&mut self, mode: u16, enable: bool) {
        match mode {
            1 => self.modes.app_cursor_keys = enable,
            5 => {
                self.modes.reverse_video = enable;
                self.buf_mut().mark_all_dirty();
            }
            6 => {
                self.modes.origin = enable;
                // DECOM homes the cursor to the region origin.
                let (top, _) = (self.buf().scroll_top, self.buf().scroll_bottom);
                let y = if enable { top } else { 0 };
                self.buf_mut().goto(0, y);
            }
            7 => self.modes.autowrap = enable,
            12 => self.modes.cursor_blink = enable,
            25 => self.modes.cursor_visible = enable,
            9 => self.modes.mouse_mode = if enable { 9 } else { 0 },
            1000 => self.modes.mouse_mode = if enable { 1000 } else { 0 },
            1002 => self.modes.mouse_mode = if enable { 1002 } else { 0 },
            1003 => self.modes.mouse_mode = if enable { 1003 } else { 0 },
            1004 => self.modes.focus_events = enable,
            1006 => self.modes.mouse_sgr = enable,
            2004 => self.modes.bracketed_paste = enable,
            47 | 1047 | 1049 => self.set_alt_screen(enable, mode == 1049),
            _ => {}
        }
    }

    fn set_alt_screen(&mut self, enable: bool, save_restore_cursor: bool) {
        if enable == self.alt_active {
            return;
        }
        if enable {
            if save_restore_cursor {
                self.save_cursor();
            }
            let pen = self.pen;
            self.alt.erase_all(pen);
            self.alt.cursor = Default::default();
            self.alt_active = true;
        } else {
            self.alt_active = false;
            if save_restore_cursor {
                self.restore_cursor();
            }
        }
        self.display_offset = 0;
        self.viewport_full = true;
        self.buf_mut().mark_all_dirty();
    }

    fn save_cursor(&mut self) {
        let c = self.buf().cursor;
        let link = self.cur_link;
        let pen = self.pen;
        self.buf_mut().saved = SavedCursor {
            x: c.x,
            y: c.y,
            pen,
            link,
        };
    }

    fn restore_cursor(&mut self) {
        let s = self.buf().saved;
        self.pen = s.pen;
        self.cur_link = s.link;
        self.buf_mut().goto(s.x, s.y);
    }

    fn set_scroll_region(&mut self, params: &Params) {
        let rows = self.rows();
        let top = params.get(0, 1).max(1) as usize - 1;
        let bottom = params.get(1, rows as u16) as usize;
        let bottom = bottom.min(rows).max(1) - 1;
        if top < bottom {
            let buf = self.buf_mut();
            buf.scroll_top = top;
            buf.scroll_bottom = bottom;
        }
        // Cursor moves to origin (home of region if DECOM, else screen home).
        let y = if self.modes.origin {
            self.buf().scroll_top
        } else {
            0
        };
        self.buf_mut().goto(0, y);
    }

    fn device_status(&mut self, params: &Params) {
        match params.get(0, 0) {
            5 => self.output.extend_from_slice(b"\x1b[0n"), // OK
            6 => {
                let (x, y) = {
                    let c = self.buf().cursor;
                    (c.x + 1, c.y + 1)
                };
                self.output
                    .extend_from_slice(format!("\x1b[{};{}R", y, x).as_bytes());
            }
            _ => {}
        }
    }

    // --- OSC ----------------------------------------------------------------

    fn osc(&mut self, params: &[&[u8]], _bell: bool) {
        let code = params.first().and_then(|p| std::str::from_utf8(p).ok());
        match code {
            Some("0") | Some("1") | Some("2") => {
                if let Some(t) = params.get(1) {
                    self.title = String::from_utf8_lossy(t).into_owned();
                    self.title_dirty = true;
                }
            }
            Some("8") => self.osc_hyperlink(params),
            Some("4") => self.osc_palette(params),
            Some("10") => self.osc_dynamic_color(params, DynColor::Fg),
            Some("11") => self.osc_dynamic_color(params, DynColor::Bg),
            Some("12") => self.osc_dynamic_color(params, DynColor::Cursor),
            Some("104") => self.osc_reset_palette(params),
            Some("110") => self.osc_reset_dynamic(DynColor::Fg),
            Some("111") => self.osc_reset_dynamic(DynColor::Bg),
            Some("112") => self.osc_reset_dynamic(DynColor::Cursor),
            // The parser split the payload on ';', but the `File=` args and the
            // base64 use ';'/':' internally — rejoin everything after the `1337`
            // code and hand the raw body to the image parser.
            Some("1337") if params.len() > 1 => {
                let body = params[1..].join(&b';');
                self.osc_iterm_image(&body);
            }
            _ => {}
        }
    }

    /// OSC 4 ; index ; spec [ ; index ; spec … ] — set palette colors. A spec of
    /// `?` queries the current value (reply via the host output stream).
    fn osc_palette(&mut self, params: &[&[u8]]) {
        let mut i = 1;
        while i + 1 < params.len() {
            let Some(idx) = std::str::from_utf8(params[i])
                .ok()
                .and_then(|s| s.trim().parse::<usize>().ok())
            else {
                i += 2;
                continue;
            };
            let spec = params[i + 1];
            if idx < 256 {
                if spec == b"?" {
                    let (r, g, b) = self.current_indexed(idx as u8);
                    self.reply_color(&format!("4;{}", idx), r, g, b);
                } else if let Some(rgb) = palette::parse_color_spec(spec) {
                    self.palette_indexed[idx] = Some(rgb);
                    self.palette_version += 1;
                }
            }
            i += 2;
        }
    }

    /// OSC 10/11/12 ; spec — set the default fg / bg / cursor color. `?` queries.
    fn osc_dynamic_color(&mut self, params: &[&[u8]], which: DynColor) {
        let Some(spec) = params.get(1) else {
            return;
        };
        if *spec == b"?" {
            let (r, g, b) = self.current_dynamic(which);
            let code = match which {
                DynColor::Fg => "10",
                DynColor::Bg => "11",
                DynColor::Cursor => "12",
            };
            self.reply_color(code, r, g, b);
            return;
        }
        if let Some(rgb) = palette::parse_color_spec(spec) {
            match which {
                DynColor::Fg => self.palette_fg = Some(rgb),
                DynColor::Bg => self.palette_bg = Some(rgb),
                DynColor::Cursor => self.palette_cursor = Some(rgb),
            }
            self.palette_version += 1;
        }
    }

    /// OSC 104 [ ; index … ] — reset palette entries (all if none given).
    fn osc_reset_palette(&mut self, params: &[&[u8]]) {
        if params.len() <= 1 {
            for e in &mut self.palette_indexed {
                *e = None;
            }
        } else {
            for p in &params[1..] {
                if let Some(idx) = std::str::from_utf8(p)
                    .ok()
                    .and_then(|s| s.trim().parse::<usize>().ok())
                {
                    if idx < 256 {
                        self.palette_indexed[idx] = None;
                    }
                }
            }
        }
        self.palette_version += 1;
    }

    /// OSC 110/111/112 — reset the default fg / bg / cursor color to the theme.
    fn osc_reset_dynamic(&mut self, which: DynColor) {
        match which {
            DynColor::Fg => self.palette_fg = None,
            DynColor::Bg => self.palette_bg = None,
            DynColor::Cursor => self.palette_cursor = None,
        }
        self.palette_version += 1;
    }

    fn current_indexed(&self, i: u8) -> (u8, u8, u8) {
        self.palette_indexed[i as usize].unwrap_or_else(|| palette::xterm256(i))
    }

    fn current_dynamic(&self, which: DynColor) -> (u8, u8, u8) {
        match which {
            DynColor::Fg => self.palette_fg.unwrap_or(self.default_fg),
            DynColor::Bg => self.palette_bg.unwrap_or(self.default_bg),
            DynColor::Cursor => self.palette_cursor.unwrap_or(self.default_cursor),
        }
    }

    /// Emit an OSC color report `ESC ] <code> ; rgb:RRRR/GGGG/BBBB ST` (the
    /// 16-bit form xterm uses; each 8-bit channel is doubled).
    fn reply_color(&mut self, code: &str, r: u8, g: u8, b: u8) {
        let msg = format!(
            "\x1b]{};rgb:{:02x}{:02x}/{:02x}{:02x}/{:02x}{:02x}\x1b\\",
            code, r, r, g, g, b, b
        );
        self.output.extend_from_slice(msg.as_bytes());
    }

    /// OSC 8 ; params ; URI — set or clear the current hyperlink.
    fn osc_hyperlink(&mut self, params: &[&[u8]]) {
        // params: ["8", "id=...", "URI..."] but the URI itself may contain ';'.
        let uri = if params.len() >= 3 {
            // Rejoin everything after the second ';' in case the URI had ';'.
            let mut joined = Vec::new();
            for (i, part) in params.iter().enumerate().skip(2) {
                if i > 2 {
                    joined.push(b';');
                }
                joined.extend_from_slice(part);
            }
            String::from_utf8_lossy(&joined).into_owned()
        } else {
            String::new()
        };

        if uri.is_empty() {
            self.cur_link = 0;
            return;
        }
        // Dedup identical consecutive URIs to bound link growth.
        if let Some(last) = self.links.last() {
            if *last == uri {
                self.cur_link = self.links.len() as u32;
                return;
            }
        }
        self.links.push(uri);
        self.cur_link = self.links.len() as u32;
    }

    fn full_reset(&mut self) {
        let cols = self.cols();
        let rows = self.rows();
        self.primary = Buffer::new(cols, rows);
        self.alt = Buffer::new(cols, rows);
        self.alt_active = false;
        self.scrollback.clear();
        self.display_offset = 0;
        self.viewport_full = true;
        self.pen = Pen::default();
        self.cur_link = 0;
        self.modes = Modes::default();
        self.links.clear();
        // NOTE: the grapheme intern table is deliberately NOT cleared here.
        // Snapshot cell `grapheme` ids must stay stable for the terminal's whole
        // lifetime so the JS-side id->cluster cache never goes stale across a
        // RIS. The table is deduplicated and each cluster is length-bounded, so
        // it can't grow unboundedly in practice.
        self.break_grapheme();
        // RIS restores the default palette.
        for e in &mut self.palette_indexed {
            *e = None;
        }
        self.palette_fg = None;
        self.palette_bg = None;
        self.palette_cursor = None;
        self.palette_version += 1;
        // Drop all images and reset the line serial.
        if !self.images.is_empty() {
            self.images.clear();
            self.images_version = self.images_version.wrapping_add(1);
        }
        self.kitty_pending = None;
        self.kitty_store.clear();
        self.scrolled_off = 0;
    }

    /// A monotonically increasing counter bumped whenever the dynamic palette
    /// (OSC 4/10/11/12/104/110/111/112) changes. The front-end compares it each
    /// frame and re-reads [`Terminal::palette_export`] when it differs.
    pub fn palette_version(&self) -> u32 {
        self.palette_version
    }

    /// Export the current palette overrides as `[fg, bg, cursor, c0..c255]`
    /// (259 words). Each word is `0` when there is no override, otherwise a
    /// packed `0x02_RRGGBB` (RGB kind, matching [`Color::pack`]). The front-end
    /// applies these on top of its theme.
    pub fn palette_export(&self) -> Vec<u32> {
        fn pack(c: Option<(u8, u8, u8)>) -> u32 {
            match c {
                Some((r, g, b)) => 0x0200_0000 | ((r as u32) << 16) | ((g as u32) << 8) | b as u32,
                None => 0,
            }
        }
        let mut out = Vec::with_capacity(259);
        out.push(pack(self.palette_fg));
        out.push(pack(self.palette_bg));
        out.push(pack(self.palette_cursor));
        for c in &self.palette_indexed {
            out.push(pack(*c));
        }
        out
    }

    /// Tell the core the theme's default fg / bg / cursor colors (packed RGB in
    /// the low 24 bits) so it can answer OSC color *queries* accurately for
    /// colors the running program has not overridden.
    pub fn set_default_colors(&mut self, fg: u32, bg: u32, cursor: u32) {
        let unpack = |c: u32| {
            (
                ((c >> 16) & 0xff) as u8,
                ((c >> 8) & 0xff) as u8,
                (c & 0xff) as u8,
            )
        };
        self.default_fg = unpack(fg);
        self.default_bg = unpack(bg);
        self.default_cursor = unpack(cursor);
    }

    // --- Sixel images -------------------------------------------------------

    /// Set the cell size in device pixels (from the front-end's font metrics),
    /// so Sixel images can be laid out and advance the cursor in whole cells.
    pub fn set_cell_pixels(&mut self, w: usize, h: usize) {
        self.cell_px_w = w.max(1);
        self.cell_px_h = h.max(1);
    }

    /// Anchor a decoded image at the cursor and move the cursor below it.
    fn place_image(&mut self, img: crate::sixel::SixelImage) {
        let rows_cells = img.height.div_ceil(self.cell_px_h).max(1);
        let col = self.buf().cursor.x;
        // Absolute serial of the cursor's row (primary screen only; on the alt
        // screen serials are still consistent because it doesn't evict).
        let serial = self.scrolled_off + self.buf().cursor.y as i64;

        let id = self.next_image_id;
        self.next_image_id = self.next_image_id.wrapping_add(1).max(1);
        self.images.push(ImageRec {
            id,
            serial,
            col,
            width: img.width,
            height: img.height,
            rows_cells,
            rgba: img.rgba,
            encoded: Vec::new(),
            mime: "",
            from_kitty: false,
            kitty_id: 0,
        });
        self.images_version = self.images_version.wrapping_add(1);
        // Bound the number of live images.
        if self.images.len() > 256 {
            self.images.remove(0);
        }

        // Move the cursor to the start of the line just below the image,
        // scrolling as needed (sixel scrolling mode).
        self.advance_below(rows_cells);
    }

    /// Move the cursor to the start of the line `rows_cells` below its current
    /// row, scrolling the buffer as needed. Shared by every inline-image path so
    /// the image occupies whole cell rows and text resumes underneath it.
    fn advance_below(&mut self, rows_cells: usize) {
        for _ in 0..rows_cells {
            self.linefeed();
        }
        self.carriage_return();
    }

    /// Handle an iTerm2 inline image: `OSC 1337 ; File=<args> : <base64>`.
    /// `body` is everything after the `1337;` code (i.e. `File=…:base64`). The
    /// image bytes are decoded and stored; the browser decodes the pixels.
    fn osc_iterm_image(&mut self, body: &[u8]) {
        // Must start with the `File=` sub-command.
        let Some(rest) = body.strip_prefix(b"File=") else {
            return;
        };
        // Split the key=value args from the base64 payload on the first ':'.
        let colon = match rest.iter().position(|&c| c == b':') {
            Some(i) => i,
            None => return,
        };
        let (args_raw, b64) = (&rest[..colon], &rest[colon + 1..]);

        let Some(bytes) = crate::img::decode_base64(b64) else {
            return;
        };
        if bytes.is_empty() {
            return;
        }

        // Parse the `;`-separated key=value arguments.
        let (mut w_arg, mut h_arg, mut preserve) =
            (crate::img::Dim::Auto, crate::img::Dim::Auto, true);
        for kv in args_raw.split(|&c| c == b';') {
            let Ok(kv) = std::str::from_utf8(kv) else {
                continue;
            };
            let Some((k, v)) = kv.split_once('=') else {
                continue;
            };
            match k.trim().to_ascii_lowercase().as_str() {
                "width" => w_arg = crate::img::Dim::parse(v),
                "height" => h_arg = crate::img::Dim::parse(v),
                "preserveaspectratio" => preserve = v.trim() != "0",
                _ => {}
            }
        }
        let mime = crate::img::detect_format(&bytes)
            .map(|f| f.mime())
            .unwrap_or("application/octet-stream");
        let (nw, nh) = crate::img::sniff_dimensions(&bytes).unwrap_or((0, 0));

        // Target display box in pixels. An axis given as `auto`/absent is left
        // to be derived; with preserveAspectRatio (the default) a single given
        // axis drives the other via the image's native ratio.
        let px_w = self.dim_to_px(w_arg, self.cell_px_w, self.cols() * self.cell_px_w);
        let px_h = self.dim_to_px(h_arg, self.cell_px_h, self.rows() * self.cell_px_h);
        let aspect = |from: usize, num: u32, den: u32| -> usize {
            if den > 0 {
                ((from as u64 * num as u64) / den as u64) as usize
            } else {
                from
            }
        };
        let (mut tw, mut th) = match (px_w, px_h, preserve) {
            (Some(w), Some(h), _) => (w, h),
            (Some(w), None, true) => (w, aspect(w, nh, nw)),
            (None, Some(h), true) => (aspect(h, nw, nh), h),
            (Some(w), None, false) => (w, nh as usize),
            (None, Some(h), false) => (nw as usize, h),
            (None, None, _) => (nw as usize, nh as usize),
        };
        // Fallback for a format whose size we can't sniff (rare): a readable
        // default box; the browser still decodes and scales into it.
        if tw == 0 || th == 0 {
            tw = (self.cols() * self.cell_px_w / 2).max(self.cell_px_w);
            th = (self.rows() * self.cell_px_h / 3).max(self.cell_px_h);
        }

        let cols_cells = tw.div_ceil(self.cell_px_w).clamp(1, self.cols().max(1));
        let rows_cells = th.div_ceil(self.cell_px_h).max(1);
        let width = cols_cells * self.cell_px_w;
        let height = rows_cells * self.cell_px_h;
        let col = self.buf().cursor.x;
        let serial = self.scrolled_off + self.buf().cursor.y as i64;

        let id = self.next_image_id;
        self.next_image_id = self.next_image_id.wrapping_add(1).max(1);
        self.images.push(ImageRec {
            id,
            serial,
            col,
            width,
            height,
            rows_cells,
            rgba: Vec::new(),
            encoded: bytes,
            mime,
            from_kitty: false,
            kitty_id: 0,
        });
        self.images_version = self.images_version.wrapping_add(1);
        if self.images.len() > 256 {
            self.images.remove(0);
        }

        self.advance_below(rows_cells);
    }

    /// Turn one iTerm2 width/height request into a pixel target, or `None` for
    /// `auto`/absent (the caller derives it). `cell_px` is the cell size on that
    /// axis (for cell counts); `axis_px` is the terminal's pixel extent on that
    /// axis (for percentages).
    fn dim_to_px(&self, dim: crate::img::Dim, cell_px: usize, axis_px: usize) -> Option<usize> {
        use crate::img::Dim;
        match dim {
            Dim::Auto => None,
            Dim::Cells(n) => Some(n as usize * cell_px.max(1)),
            Dim::Pixels(px) => Some(px as usize),
            Dim::Percent(p) => Some((axis_px * p as usize) / 100),
        }
    }

    // --- Kitty graphics protocol (APC `_G…`) --------------------------------

    /// Handle one Kitty graphics APC command (payload after `ESC _`, starting
    /// with `G`). See [`crate::kitty`] for the supported subset.
    fn kitty(&mut self, data: &[u8]) {
        let Some((cmd, b64)) = crate::kitty::parse(data) else {
            return;
        };
        match cmd.action {
            b'q' => {
                // Query: acknowledge so a client can probe support.
                self.kitty_reply(cmd.id, cmd.quiet, "OK");
                return;
            }
            b'd' => {
                self.kitty_delete(&cmd);
                return;
            }
            // Display a stored image with no new data.
            b'p' if b64.is_empty() && !cmd.more => {
                if let Some(pos) = self.kitty_store.iter().position(|im| im.id == cmd.id) {
                    let img = self.kitty_store[pos].clone();
                    self.place_kitty(&img, &cmd);
                    if cmd.id != 0 && cmd.quiet == 0 {
                        self.kitty_reply(cmd.id, cmd.quiet, "OK");
                    }
                }
                return;
            }
            _ => {}
        }

        // Transmission (a=t / a=T / a=p-with-data). Accumulate this chunk; the
        // first chunk's control block governs the whole transfer.
        if self.kitty_pending.is_none() {
            self.kitty_pending = Some((cmd.clone(), Vec::new()));
        }
        if let Some((_, buf)) = self.kitty_pending.as_mut() {
            buf.extend_from_slice(b64);
            if buf.len() > MAX_KITTY_B64 {
                self.kitty_pending = None; // hostile / oversized transfer
                return;
            }
        }
        if cmd.more {
            return; // more chunks to come
        }
        let Some((first, b64all)) = self.kitty_pending.take() else {
            return;
        };

        // Only direct base64 transmission is honored; file / shared-memory media
        // and zlib compression are refused (see `kitty` module docs).
        if first.medium != b'd' || first.compressed {
            return;
        }
        let Some(bytes) = crate::img::decode_base64(&b64all) else {
            return;
        };
        if bytes.is_empty() {
            return;
        }

        let img = match first.format {
            24 | 32 => {
                let (w, h) = (first.width as usize, first.height as usize);
                let bpp = if first.format == 24 { 3 } else { 4 };
                if w == 0 || h == 0 || w > 1 << 16 || h > 1 << 16 || bytes.len() < w * h * bpp {
                    return;
                }
                let data = if first.format == 24 {
                    let mut rgba = Vec::with_capacity(w * h * 4);
                    for px in bytes[..w * h * 3].chunks_exact(3) {
                        rgba.extend_from_slice(&[px[0], px[1], px[2], 0xff]);
                    }
                    rgba
                } else {
                    bytes[..w * h * 4].to_vec()
                };
                KittyImage {
                    id: first.id,
                    format: 32,
                    width: w as u32,
                    height: h as u32,
                    data,
                }
            }
            100 => {
                // PNG: let the front-end decode it. Sniff native size for layout.
                let (w, h) = crate::img::sniff_dimensions(&bytes).unwrap_or((0, 0));
                KittyImage {
                    id: first.id,
                    format: 100,
                    width: w,
                    height: h,
                    data: bytes,
                }
            }
            _ => return, // unknown pixel format
        };

        let display = matches!(first.action, b'T' | b'p');
        if display {
            self.place_kitty(&img, &first);
        }
        if first.id != 0 {
            self.kitty_remember(img);
        }
        if first.id != 0 && first.quiet == 0 {
            self.kitty_reply(first.id, first.quiet, "OK");
        }
    }

    /// Anchor a normalized Kitty image at the cursor and move the cursor below
    /// it. Raw RGBA draws 1:1 at native pixels (like Sixel); a PNG is handed to
    /// the front-end and laid into a cell box (explicit `c`/`r`, else derived).
    fn place_kitty(&mut self, img: &KittyImage, cmd: &crate::kitty::Cmd) {
        let col = self.buf().cursor.x;
        let serial = self.scrolled_off + self.buf().cursor.y as i64;
        let id = self.next_image_id;
        self.next_image_id = self.next_image_id.wrapping_add(1).max(1);

        let (width, height, rows_cells, rgba, encoded, mime) = if img.format == 100 {
            let (cols_cells, rows_cells) =
                self.kitty_cell_box(cmd, img.width as usize, img.height as usize);
            (
                cols_cells * self.cell_px_w,
                rows_cells * self.cell_px_h,
                rows_cells,
                Vec::new(),
                img.data.clone(),
                "image/png",
            )
        } else {
            let (w, h) = (img.width as usize, img.height as usize);
            let rows_cells = h.div_ceil(self.cell_px_h).max(1);
            (w, h, rows_cells, img.data.clone(), Vec::new(), "")
        };

        self.images.push(ImageRec {
            id,
            serial,
            col,
            width,
            height,
            rows_cells,
            rgba,
            encoded,
            mime,
            from_kitty: true,
            kitty_id: img.id,
        });
        self.images_version = self.images_version.wrapping_add(1);
        if self.images.len() > 256 {
            self.images.remove(0);
        }
        self.advance_below(rows_cells);
    }

    /// Display size in whole cells for a Kitty image: explicit `c`/`r` win, else
    /// derive from the native pixel size, clamped to the screen width.
    fn kitty_cell_box(&self, cmd: &crate::kitty::Cmd, nw: usize, nh: usize) -> (usize, usize) {
        let cols_cells = if cmd.cols > 0 {
            cmd.cols as usize
        } else if nw > 0 {
            nw.div_ceil(self.cell_px_w)
        } else {
            (self.cols() / 2).max(1)
        };
        let rows_cells = if cmd.rows > 0 {
            cmd.rows as usize
        } else if nh > 0 {
            nh.div_ceil(self.cell_px_h)
        } else {
            (self.rows() / 3).max(1)
        };
        (cols_cells.clamp(1, self.cols().max(1)), rows_cells.max(1))
    }

    /// Store a transmitted image for a later `a=p`, replacing any same-id entry
    /// and bounding the store by count and total bytes.
    fn kitty_remember(&mut self, img: KittyImage) {
        self.kitty_store.retain(|im| im.id != img.id);
        self.kitty_store.push(img);
        while self.kitty_store.len() > 32 {
            self.kitty_store.remove(0);
        }
        let mut total: usize = self.kitty_store.iter().map(|im| im.data.len()).sum();
        while total > MAX_KITTY_STORE && self.kitty_store.len() > 1 {
            total -= self.kitty_store.remove(0).data.len();
        }
    }

    /// Delete Kitty images. `d=i`/`I` targets one client id; anything else
    /// clears all Kitty images. Sixel / iTerm2 images are never touched.
    fn kitty_delete(&mut self, cmd: &crate::kitty::Cmd) {
        let before = self.images.len();
        match cmd.delete {
            b'i' | b'I' if cmd.id != 0 => {
                self.images
                    .retain(|im| !(im.from_kitty && im.kitty_id == cmd.id));
                self.kitty_store.retain(|im| im.id != cmd.id);
            }
            _ => {
                self.images.retain(|im| !im.from_kitty);
                self.kitty_store.clear();
            }
        }
        if self.images.len() != before {
            self.images_version = self.images_version.wrapping_add(1);
        }
    }

    /// Emit a Kitty response (`ESC _ G i=<id>;<msg> ESC \`) unless suppressed.
    fn kitty_reply(&mut self, id: u32, quiet: u32, msg: &str) {
        if quiet >= 2 {
            return;
        }
        let s = if id != 0 {
            format!("\x1b_Gi={};{}\x1b\\", id, msg)
        } else {
            format!("\x1b_G;{}\x1b\\", msg)
        };
        self.output.extend_from_slice(s.as_bytes());
    }

    /// Drop images that have scrolled entirely out of retained scrollback.
    fn prune_images(&mut self) {
        let oldest = self.scrolled_off - self.max_scrollback as i64;
        let before = self.images.len();
        self.images
            .retain(|im| im.serial + im.rows_cells as i64 > oldest);
        if self.images.len() != before {
            self.images_version = self.images_version.wrapping_add(1);
        }
    }

    /// Drop images whose rows intersect the current primary screen (used by
    /// full-screen erase / alt-screen switch).
    fn clear_screen_images(&mut self) {
        let top = self.scrolled_off;
        let bottom = self.scrolled_off + self.rows() as i64;
        let before = self.images.len();
        self.images
            .retain(|im| im.serial + im.rows_cells as i64 <= top || im.serial >= bottom);
        if self.images.len() != before {
            self.images_version = self.images_version.wrapping_add(1);
        }
    }

    /// Counter bumped whenever the image set changes; the front-end re-syncs its
    /// texture cache when it differs.
    pub fn images_version(&self) -> u32 {
        self.images_version
    }

    /// Live image ids, in draw order (oldest first).
    pub fn image_ids(&self) -> Vec<u32> {
        self.images.iter().map(|im| im.id).collect()
    }

    /// RGBA bytes of image `id` (`width*height*4`), or empty if gone.
    pub fn image_rgba(&self, id: u32) -> Vec<u8> {
        self.images
            .iter()
            .find(|im| im.id == id)
            .map(|im| im.rgba.clone())
            .unwrap_or_default()
    }

    /// Raw encoded image-file bytes for image `id` (iTerm2 path), or empty for a
    /// Sixel/RGBA image or a missing id. The front-end decodes these natively.
    pub fn image_encoded(&self, id: u32) -> Vec<u8> {
        self.images
            .iter()
            .find(|im| im.id == id)
            .map(|im| im.encoded.clone())
            .unwrap_or_default()
    }

    /// MIME hint for image `id`'s encoded bytes, or empty for the RGBA path.
    pub fn image_mime(&self, id: u32) -> String {
        self.images
            .iter()
            .find(|im| im.id == id)
            .map(|im| im.mime.to_string())
            .unwrap_or_default()
    }

    /// `(width, height)` in pixels of image `id`, or `(0, 0)`.
    pub fn image_size(&self, id: u32) -> Vec<u32> {
        self.images
            .iter()
            .find(|im| im.id == id)
            .map(|im| vec![im.width as u32, im.height as u32])
            .unwrap_or_else(|| vec![0, 0])
    }

    /// Current placement of every live image relative to the visible viewport,
    /// flat `[id, viewport_row, col, width_px, height_px] …`. `viewport_row` is
    /// the cell row of the image's top edge (may be negative / off-screen); the
    /// front-end multiplies by the cell size for the pixel offset. Recomputed
    /// each frame so images track scrolling.
    pub fn image_placements(&self) -> Vec<i32> {
        // Images belong to the primary screen; hide them while the alternate
        // screen is active (they reappear on return since serials are absolute).
        if self.alt_active {
            return Vec::new();
        }
        let top_serial = self.scrolled_off - self.display_offset as i64;
        let mut out = Vec::with_capacity(self.images.len() * 5);
        for im in &self.images {
            out.push(im.id as i32);
            out.push((im.serial - top_serial) as i32);
            out.push(im.col as i32);
            out.push(im.width as i32);
            out.push(im.height as i32);
        }
        out
    }

    /// Resolve a grapheme-cluster id (as emitted in the snapshot) to its full
    /// cluster string. Returns `None` for id 0 or an out-of-range id.
    pub fn grapheme(&self, id: u32) -> Option<&str> {
        if id == 0 {
            return None;
        }
        self.graphemes.get((id - 1) as usize).map(|s| s.as_str())
    }

    // --- snapshot -----------------------------------------------------------

    /// Produce a render snapshot. When `force` is `true` (or the viewport
    /// changed) every row is emitted; otherwise only dirty rows are.
    ///
    /// Layout (all `u32`):
    /// `[MAGIC, cols, rows, cur_x, cur_y, cur_flags, n_rows]`
    /// then per row: `[row_index, cell*cols]` where each cell is
    /// `[codepoint, fg, bg, flags, link, grapheme]`. `grapheme` is 0 when the
    /// cell is a single scalar (`codepoint`); non-zero ids resolve to the full
    /// cluster string via [`Terminal::grapheme`].
    pub fn snapshot(&mut self, force: bool) -> Vec<u32> {
        let mut out = Vec::new();
        self.snapshot_into(force, &mut out);
        out
    }

    /// Like [`snapshot`], but fills a caller-provided buffer (cleared first) so
    /// its capacity is reused across frames — no per-frame allocation. The wasm
    /// binding uses this to hand JavaScript a zero-copy view of the packed data.
    pub fn snapshot_into(&mut self, force: bool, out: &mut Vec<u32>) {
        let force = force || self.viewport_full;
        let cols = self.cols();
        let rows = self.rows();
        let total_back = self.scrollback.len();
        let offset = self.display_offset;

        out.clear();
        out.reserve(7 + rows * (1 + cols * SNAPSHOT_CELL_WORDS));
        out.push(SNAPSHOT_MAGIC);
        out.push(cols as u32);
        out.push(rows as u32);

        let (cx, cy) = {
            let c = self.buf().cursor;
            (c.x as u32, c.y as u32)
        };
        out.push(cx);
        out.push(cy);
        let mut cflags = 0u32;
        if self.modes.cursor_visible {
            cflags |= 1;
        }
        if self.modes.cursor_blink {
            cflags |= 2;
        }
        if offset == 0 {
            cflags |= 4; // cursor is on the visible screen
        }
        out.push(cflags);

        // Reserve n_rows slot; fill after.
        let n_idx = out.len();
        out.push(0);

        let mut n = 0u32;
        for y in 0..rows {
            let dirty = force || self.viewport_row_dirty(y);
            if !dirty {
                continue;
            }
            out.push(y as u32);
            n += 1;
            // Resolve the visible line for row y (scrollback or grid).
            let idx = total_back as isize - offset as isize + y as isize;
            if idx >= 0 && (idx as usize) < total_back {
                let line = &self.scrollback[idx as usize];
                push_line(out, line, cols);
            } else {
                let gy = (idx - total_back as isize) as usize;
                let line = self.buf().line(gy.min(rows - 1));
                push_line(out, line, cols);
            }
        }
        out[n_idx] = n;

        // Consume dirty state.
        self.primary.clear_dirty();
        self.alt.clear_dirty();
        self.viewport_full = false;
    }

    /// Whether row `y` of the *viewport* needs redraw. When scrolled into
    /// history the grid's dirty flags don't apply, so `viewport_full` (checked
    /// by the caller) handles that; here we only forward grid dirtiness when at
    /// the bottom.
    fn viewport_row_dirty(&self, y: usize) -> bool {
        if self.display_offset != 0 {
            return false; // caller forces full redraw via `viewport_full`
        }
        self.buf().is_dirty(y)
    }
}

fn push_line(out: &mut Vec<u32>, line: &Line, cols: usize) {
    for x in 0..cols {
        let cell = line.get(x).copied().unwrap_or_default();
        out.push(cell.ch as u32);
        out.push(cell.pen.fg.pack());
        out.push(cell.pen.bg.pack());
        out.push(cell.pen.flags as u32);
        out.push(cell.link);
        out.push(cell.grapheme);
    }
}

/// Parse an extended SGR color starting at param group `i` (which is `38`/`48`).
/// Supports both `38;5;n` / `38;2;r;g;b` (semicolon) and the `38:2::r:g:b`
/// (colon sub-parameter) forms. Returns the color and how many *extra* groups
/// were consumed.
fn parse_ext_color(params: &Params, i: usize) -> Option<(Color, usize)> {
    // Colon form: everything is a sub-parameter of group `i`.
    if params.get_sub(i, 1).is_some() {
        let kind = params.get_sub(i, 1)?;
        return match kind {
            5 => {
                let idx = params.get_sub(i, 2)? as u8;
                Some((Color::Indexed(idx), 0))
            }
            2 => {
                // 38:2:<colorspace>:r:g:b  — colorspace slot may be present.
                let (r, g, b) = if params.get_sub(i, 5).is_some() {
                    (
                        params.get_sub(i, 3)? as u8,
                        params.get_sub(i, 4)? as u8,
                        params.get_sub(i, 5)? as u8,
                    )
                } else {
                    (
                        params.get_sub(i, 2)? as u8,
                        params.get_sub(i, 3)? as u8,
                        params.get_sub(i, 4)? as u8,
                    )
                };
                Some((Color::Rgb(r, g, b), 0))
            }
            _ => None,
        };
    }

    // Semicolon form: subsequent groups.
    match params.get(i + 1, 0) {
        5 => {
            let idx = params.get(i + 2, 0) as u8;
            Some((Color::Indexed(idx), 2))
        }
        2 => {
            let r = params.get(i + 2, 0) as u8;
            let g = params.get(i + 3, 0) as u8;
            let b = params.get(i + 4, 0) as u8;
            Some((Color::Rgb(r, g, b), 4))
        }
        _ => None,
    }
}

fn is_regional_indicator(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

impl Perform for Terminal {
    fn print(&mut self, c: char) {
        self.write_char(c);
    }

    /// Fast path for runs of printable ASCII (all width-1). Fills whole spans of
    /// a line in a tight loop instead of dispatching per character.
    fn print_ascii(&mut self, mut bytes: &[u8]) {
        // ASCII is never combining/ZWJ/RI, but a following combining mark should
        // attach to the last ASCII cell, so keep last_grapheme current.
        self.pending_zwj = false;
        self.last_ri = false;
        if self.modes.insert {
            for &b in bytes {
                self.write_char(b as char);
            }
            return;
        }
        let cols = self.cols();
        let autowrap = self.modes.autowrap;
        let pen = self.pen;
        let link = self.cur_link;
        while !bytes.is_empty() {
            if self.buf().cursor.pending_wrap && autowrap {
                self.buf_mut().cursor.pending_wrap = false;
                let y = self.buf().cursor.y;
                self.buf_mut().mark_wrapped(y);
                self.carriage_return();
                self.linefeed();
            }
            let (x, y) = {
                let c = self.buf().cursor;
                (c.x, c.y)
            };
            let space = cols - x;
            let n = space.min(bytes.len());
            {
                let line = self.buf_mut().line_mut(y);
                for (k, &b) in bytes[..n].iter().enumerate() {
                    line[x + k] = Cell {
                        ch: b as char,
                        pen,
                        link,
                        grapheme: 0,
                    };
                }
            }
            self.last_grapheme = Some((x + n - 1, y));
            bytes = &bytes[n..];
            let buf = self.buf_mut();
            let nx = x + n;
            if nx >= cols {
                buf.cursor.x = cols - 1;
                buf.cursor.pending_wrap = autowrap;
                if bytes.is_empty() {
                    break;
                }
                // Bytes remain: if autowrap, the next iteration wraps via the
                // pending flag; if not, it overwrites the last column.
            } else {
                buf.cursor.x = nx;
                break;
            }
        }
    }

    fn execute(&mut self, byte: u8) {
        // Any C0 control breaks grapheme continuation: a combining mark after
        // CR/LF/BS/HT must not attach across the cursor discontinuity. (BEL is
        // harmless to break on too.)
        self.break_grapheme();
        match byte {
            0x07 => self.bell_count += 1, // BEL
            0x08 => {
                // BS
                let x = self.buf().cursor.x;
                if x > 0 {
                    self.buf_mut().cursor.x = x - 1;
                }
                self.buf_mut().cursor.pending_wrap = false;
            }
            0x09 => {
                // HT — next 8-column tab stop
                let cols = self.cols();
                let x = self.buf().cursor.x;
                let next = ((x / 8) + 1) * 8;
                self.buf_mut().goto_col(next.min(cols - 1));
            }
            0x0a..=0x0c => {
                // LF / VT / FF
                self.linefeed();
                if self.modes.newline_mode {
                    self.carriage_return();
                }
            }
            0x0d => self.carriage_return(), // CR
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        if ignore {
            return;
        }
        // A CSI (cursor move, erase, SGR, …) ends any in-progress cluster.
        self.break_grapheme();
        let private = intermediates.first() == Some(&b'?');
        let rows = self.rows();
        let cols = self.cols();
        let cur = self.buf().cursor;

        match action {
            'A' => {
                let n = params.get(0, 1).max(1) as usize;
                let top = self.buf().scroll_top;
                let ny = cur.y.saturating_sub(n).max(top);
                self.buf_mut().goto_row(ny);
            }
            'B' | 'e' => {
                let n = params.get(0, 1).max(1) as usize;
                let bottom = self.buf().scroll_bottom;
                let ny = (cur.y + n).min(bottom);
                self.buf_mut().goto_row(ny);
            }
            'C' | 'a' => {
                let n = params.get(0, 1).max(1) as usize;
                self.buf_mut().goto_col((cur.x + n).min(cols - 1));
            }
            'D' => {
                let n = params.get(0, 1).max(1) as usize;
                self.buf_mut().goto_col(cur.x.saturating_sub(n));
            }
            'E' => {
                let n = params.get(0, 1).max(1) as usize;
                let bottom = self.buf().scroll_bottom;
                let ny = (cur.y + n).min(bottom);
                self.buf_mut().goto(0, ny);
            }
            'F' => {
                let n = params.get(0, 1).max(1) as usize;
                let top = self.buf().scroll_top;
                let ny = cur.y.saturating_sub(n).max(top);
                self.buf_mut().goto(0, ny);
            }
            'G' | '`' => {
                let x = params.get(0, 1).max(1) as usize - 1;
                self.buf_mut().goto_col(x);
            }
            'd' => {
                let y = params.get(0, 1).max(1) as usize - 1;
                self.buf_mut().goto_row(y);
            }
            'H' | 'f' => {
                let mut y = params.get(0, 1).max(1) as usize - 1;
                let x = params.get(1, 1).max(1) as usize - 1;
                if self.modes.origin {
                    y += self.buf().scroll_top;
                    y = y.min(self.buf().scroll_bottom);
                }
                self.buf_mut().goto(x, y);
            }
            'J' => {
                let pen = self.pen;
                match params.get(0, 0) {
                    0 => self.buf_mut().erase_below(pen),
                    1 => self.buf_mut().erase_above(pen),
                    2 | 3 => {
                        self.buf_mut().erase_all(pen);
                        if !self.alt_active {
                            self.clear_screen_images();
                        }
                    }
                    _ => {}
                }
            }
            'K' => {
                let pen = self.pen;
                match params.get(0, 0) {
                    0 => self.buf_mut().erase_line_to_right(pen),
                    1 => self.buf_mut().erase_line_to_left(pen),
                    2 => self.buf_mut().erase_whole_line(pen),
                    _ => {}
                }
            }
            'L' => {
                let n = params.get(0, 1).max(1) as usize;
                let pen = self.pen;
                self.buf_mut().insert_lines(n, pen);
            }
            'M' => {
                let n = params.get(0, 1).max(1) as usize;
                let pen = self.pen;
                self.buf_mut().delete_lines(n, pen);
            }
            'P' => {
                let n = params.get(0, 1).max(1) as usize;
                let pen = self.pen;
                self.buf_mut().delete_chars(n, pen);
            }
            '@' => {
                let n = params.get(0, 1).max(1) as usize;
                let pen = self.pen;
                self.buf_mut().insert_chars(n, pen);
            }
            'X' => {
                let n = params.get(0, 1).max(1) as usize;
                let pen = self.pen;
                self.buf_mut().erase_chars(n, pen);
            }
            'S' => {
                let n = params.get(0, 1).max(1) as usize;
                self.scroll_region_up(n);
            }
            'T' => {
                let n = params.get(0, 1).max(1) as usize;
                let pen = self.pen;
                self.buf_mut().scroll_down(n, pen);
            }
            'm' => self.sgr(params),
            'r' => self.set_scroll_region(params),
            'h' => self.set_mode(params, private, true),
            'l' => self.set_mode(params, private, false),
            'n' => self.device_status(params),
            'c' => {
                // Primary Device Attributes: VT100 with advanced video.
                self.output.extend_from_slice(b"\x1b[?1;2c");
            }
            's' => self.save_cursor(),
            'u' => self.restore_cursor(),
            _ => {
                let _ = rows;
            }
        }
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], byte: u8) {
        self.break_grapheme();
        if intermediates.is_empty() {
            match byte {
                b'D' => self.linefeed(), // IND
                b'E' => {
                    self.carriage_return();
                    self.linefeed();
                } // NEL
                b'M' => self.reverse_index(), // RI
                b'7' => self.save_cursor(), // DECSC
                b'8' => self.restore_cursor(), // DECRC
                b'c' => self.full_reset(), // RIS
                b'=' => self.modes.app_keypad = true,
                b'>' => self.modes.app_keypad = false,
                _ => {}
            }
        }
        // Charset designation (`( ) * +`) and others are accepted and ignored.
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell: bool) {
        self.osc(params, bell);
    }

    fn dcs_dispatch(&mut self, data: &[u8], _truncated: bool) {
        if let Some(img) = crate::sixel::decode(data) {
            self.place_image(img);
        }
    }

    fn apc_dispatch(&mut self, data: &[u8]) {
        self.kitty(data);
    }
}
