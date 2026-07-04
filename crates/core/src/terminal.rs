//! The terminal state machine: implements [`Perform`] to turn parser tokens
//! into buffer mutations, tracks modes, scrollback and the alt-screen, resolves
//! OSC 8 hyperlinks, and produces render snapshots.

use std::collections::VecDeque;

use crate::cell::{attr, Cell, Color, Pen};
use crate::grid::{Buffer, Line, SavedCursor};
use crate::parser::{Params, Parser, Perform};
use crate::width::char_width;

/// The render-snapshot magic word (little-endian `F3E7` + version).
pub const SNAPSHOT_MAGIC: u32 = 0xF3E7_0001;
/// Words per cell in a snapshot: `[codepoint, fg, bg, flags, link]`.
pub const SNAPSHOT_CELL_WORDS: usize = 6;

/// DEC private + ANSI modes we track. Exposed to the host so it can encode
/// keyboard and mouse input the way the running program expects.
#[derive(Clone, Copy, Debug)]
pub struct Modes {
    pub autowrap: bool,          // DECAWM (7)
    pub cursor_visible: bool,    // DECTCEM (25)
    pub cursor_blink: bool,      // (12)
    pub app_cursor_keys: bool,   // DECCKM (1)
    pub app_keypad: bool,        // DECKPAM
    pub insert: bool,            // IRM (4)
    pub newline_mode: bool,      // LNM (20)
    pub bracketed_paste: bool,   // (2004)
    pub focus_events: bool,      // (1004)
    pub origin: bool,            // DECOM (6)
    /// Mouse tracking: 0=off, 9=X10, 1000=button, 1002=drag, 1003=any.
    pub mouse_mode: u16,
    /// Mouse encoding: 0=default, 1006=SGR.
    pub mouse_sgr: bool,
    pub reverse_video: bool,     // DECSCNM (5)
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
}

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

    /// Text of logical line `abs` (0 = oldest scrollback line). Wide-glyph
    /// spacer cells are skipped so the string reads naturally.
    pub fn line_text(&self, abs: usize) -> String {
        let back = self.scrollback.len();
        let line = if abs < back {
            &self.scrollback[abs]
        } else {
            self.buf().line((abs - back).min(self.rows() - 1))
        };
        line.iter()
            .filter(|c| !c.pen.has(attr::WIDE_SPACER))
            .map(|c| c.ch)
            .collect::<String>()
            .trim_end()
            .to_string()
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
        self.primary.resize(cols, rows);
        self.alt.resize(cols, rows);
        self.display_offset = 0;
        self.viewport_full = true;
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
            self.carriage_return();
            self.linefeed();
        }

        // A wide glyph that won't fit in the last column wraps first.
        if w == 2 && self.buf().cursor.x == cols - 1 {
            if autowrap {
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
        let new = Cell { grapheme: id, ..cell };
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
            for line in evicted {
                self.scrollback.push_back(line);
                while self.scrollback.len() > self.max_scrollback {
                    self.scrollback.pop_front();
                }
            }
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
        let y = if self.modes.origin { self.buf().scroll_top } else { 0 };
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
            _ => {}
        }
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
        let force = force || self.viewport_full;
        let cols = self.cols();
        let rows = self.rows();
        let total_back = self.scrollback.len();
        let offset = self.display_offset;

        let mut out = Vec::with_capacity(7 + rows * (1 + cols * SNAPSHOT_CELL_WORDS));
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
                push_line(&mut out, line, cols);
            } else {
                let gy = (idx - total_back as isize) as usize;
                let line = self.buf().line(gy.min(rows - 1));
                push_line(&mut out, line, cols);
            }
        }
        out[n_idx] = n;

        // Consume dirty state.
        self.primary.clear_dirty();
        self.alt.clear_dirty();
        self.viewport_full = false;
        out
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
            0x07 => self.bell_count += 1,       // BEL
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
                    2 | 3 => self.buf_mut().erase_all(pen),
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
                b'D' => self.linefeed(),          // IND
                b'E' => {
                    self.carriage_return();
                    self.linefeed();
                } // NEL
                b'M' => self.reverse_index(),     // RI
                b'7' => self.save_cursor(),       // DECSC
                b'8' => self.restore_cursor(),    // DECRC
                b'c' => self.full_reset(),        // RIS
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
}
