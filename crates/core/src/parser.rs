//! A from-scratch ANSI / VT-500 escape-sequence parser.
//!
//! The control structure follows Paul Williams' well-known DEC parser state
//! diagram (<https://vt100.net/emu/dec_ansi_parser>), extended with a UTF-8
//! decoder in the ground state so that printable text is delivered as `char`s.
//!
//! The parser is transport-agnostic: it turns a byte stream into semantic
//! callbacks on a [`Perform`] implementor. All terminal *behavior* lives in the
//! performer ([`crate::terminal::Terminal`]); this file only classifies bytes.

/// Maximum number of `;`/`:`-separated parameters we retain in a single
/// CSI/OSC sequence. Anything beyond this is dropped (matches xterm's cap and
/// prevents unbounded growth from malicious input).
const MAX_PARAMS: usize = 32;
/// Maximum sub-parameters (`:`-separated) within one parameter group.
const MAX_SUBPARAMS: usize = 8;
/// Maximum intermediate bytes retained.
const MAX_INTERMEDIATES: usize = 2;
/// Cap on an OSC string payload to bound memory from a runaway sequence.
const MAX_OSC_LEN: usize = 8 * 1024;

/// Numeric parameters of a CSI/DCS sequence.
///
/// Each entry is a *group*: the value plus any `:`-separated sub-parameters
/// (used by the ITU/`38:2::r:g:b` true-color SGR form). Most callers only need
/// [`Params::get`].
#[derive(Debug, Default)]
pub struct Params {
    groups: Vec<Vec<u16>>,
}

impl Params {
    fn clear(&mut self) {
        self.groups.clear();
    }

    fn ensure_first(&mut self) {
        if self.groups.is_empty() {
            self.groups.push(vec![0]);
        }
    }

    fn push_group(&mut self) {
        if self.groups.len() < MAX_PARAMS {
            self.groups.push(vec![0]);
        }
    }

    fn push_subparam(&mut self) {
        self.ensure_first();
        if let Some(last) = self.groups.last_mut() {
            if last.len() < MAX_SUBPARAMS {
                last.push(0);
            }
        }
    }

    fn add_digit(&mut self, d: u16) {
        self.ensure_first();
        if let Some(v) = self.groups.last_mut().and_then(|g| g.last_mut()) {
            *v = v.saturating_mul(10).saturating_add(d);
        }
    }

    /// Number of parameter groups present.
    pub fn len(&self) -> usize {
        self.groups.len()
    }

    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// The primary value of group `i`, or `default` if the group is absent or
    /// was left empty (a bare `;`, which conventionally means "default").
    pub fn get(&self, i: usize, default: u16) -> u16 {
        match self.groups.get(i).and_then(|g| g.first()) {
            Some(&0) if self.is_defaulted(i) => default,
            Some(&v) => v,
            None => default,
        }
    }

    /// The `j`-th sub-parameter of group `i`, if present.
    pub fn get_sub(&self, i: usize, j: usize) -> Option<u16> {
        self.groups.get(i).and_then(|g| g.get(j)).copied()
    }

    /// Whether group `i` was written as an explicit `0` vs. left empty.
    /// We can't distinguish after the fact, so `0` always yields `default`
    /// via [`get`] — matching how real terminals treat `CSI 0 m` == `CSI m`.
    fn is_defaulted(&self, _i: usize) -> bool {
        true
    }

    /// Iterate the primary value of each group in order.
    pub fn iter(&self) -> impl Iterator<Item = u16> + '_ {
        self.groups.iter().filter_map(|g| g.first().copied())
    }
}

/// Callbacks invoked as the parser recognizes tokens.
pub trait Perform {
    /// A printable character was decoded (already UTF-8 decoded, >= U+0020).
    fn print(&mut self, c: char);
    /// A run of printable ASCII bytes (0x20..0x7e). The default forwards to
    /// [`print`]; implementors can override for a faster bulk path. This is the
    /// hot path for ordinary text output.
    fn print_ascii(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.print(b as char);
        }
    }
    /// A C0/C1 control byte to execute (BEL, BS, HT, LF, CR, ...).
    fn execute(&mut self, byte: u8);
    /// A final CSI byte: `ESC [ params intermediates action`.
    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char);
    /// A final ESC byte: `ESC intermediates byte`.
    fn esc_dispatch(&mut self, intermediates: &[u8], byte: u8);
    /// A completed OSC (`ESC ] ... BEL` / `... ST`). `params` is the payload
    /// split on `;`.
    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool);
    /// A completed DCS (`ESC P ... ST`). `data` is the raw payload after `ESC P`
    /// (the leading params/intermediates + final byte + string). The default
    /// ignores it. Used for Sixel (`... q ...`). `truncated` is set if the
    /// payload hit the parser's size cap.
    fn dcs_dispatch(&mut self, _data: &[u8], _truncated: bool) {}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    OscString,
    // DCS and SOS/PM/APC are consumed but not acted upon.
    DcsPassthrough,
    SosPmApcString,
}

/// The escape-sequence parser. Feed bytes with [`Parser::advance`].
pub struct Parser {
    state: State,
    params: Params,
    intermediates: Vec<u8>,
    ignoring: bool,
    osc_raw: Vec<u8>,
    /// Raw DCS payload (everything after `ESC P`, up to ST). Capped to bound
    /// memory against a hostile un-terminated sequence.
    dcs_raw: Vec<u8>,
    dcs_overflow: bool,
    // UTF-8 decode state (only used in Ground).
    utf8_remaining: u8,
    utf8_cp: u32,
    utf8_min: u32,
}

/// Cap on captured DCS payload (Sixel images can be large, but bound it).
const MAX_DCS: usize = 8 * 1024 * 1024;

impl Default for Parser {
    fn default() -> Self {
        Parser::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Parser {
            state: State::Ground,
            params: Params::default(),
            intermediates: Vec::with_capacity(MAX_INTERMEDIATES),
            ignoring: false,
            osc_raw: Vec::new(),
            dcs_raw: Vec::new(),
            dcs_overflow: false,
            utf8_remaining: 0,
            utf8_cp: 0,
            utf8_min: 0,
        }
    }

    /// Feed a chunk of bytes, driving callbacks on `perform`.
    ///
    /// Fast path: while in the ground state with no pending UTF-8, a run of
    /// printable ASCII is handed to [`Perform::print_ascii`] in one call,
    /// skipping the per-byte state machine for ordinary text (the common case).
    pub fn advance<P: Perform>(&mut self, perform: &mut P, bytes: &[u8]) {
        let len = bytes.len();
        let mut i = 0;
        while i < len {
            if self.state == State::Ground && self.utf8_remaining == 0 {
                let start = i;
                while i < len {
                    if !(0x20..0x7f).contains(&bytes[i]) {
                        break;
                    }
                    i += 1;
                }
                if i > start {
                    perform.print_ascii(&bytes[start..i]);
                    continue;
                }
            }
            self.step(perform, bytes[i]);
            i += 1;
        }
    }

    fn step<P: Perform>(&mut self, perform: &mut P, b: u8) {
        // UTF-8 continuation handling takes precedence, but only in Ground.
        if self.utf8_remaining > 0 {
            if self.state == State::Ground {
                self.utf8_continue(perform, b);
                return;
            }
            // A control interrupted a multibyte char; drop the partial.
            self.utf8_remaining = 0;
        }

        // C0 controls (except ESC) and DEL are handled per the "anywhere"
        // rules of the DEC diagram, with a couple of state-specific exceptions.
        match b {
            0x1b => {
                // ESC restarts an escape sequence. If we were inside a string
                // (OSC/DCS/SOS), this ESC is the lead byte of the ST (ESC \)
                // terminator — flush the OSC before restarting.
                if self.state == State::OscString {
                    self.osc_end(perform, false);
                } else if self.state == State::DcsPassthrough {
                    self.dcs_end(perform);
                }
                self.enter_escape();
                return;
            }
            0x18 | 0x1a => {
                // CAN / SUB abort the current sequence.
                self.state = State::Ground;
                return;
            }
            0x07 if self.state == State::OscString => {
                // BEL terminates an OSC string.
                self.osc_end(perform, true);
                return;
            }
            0x00..=0x17 | 0x19 | 0x1c..=0x1f => {
                match self.state {
                    // C0 controls inside a string are ignored (not stored).
                    State::OscString | State::DcsPassthrough | State::SosPmApcString => {}
                    _ => perform.execute(b),
                }
                return;
            }
            _ => {}
        }

        match self.state {
            State::Ground => self.ground(perform, b),
            State::Escape => self.escape(perform, b),
            State::EscapeIntermediate => self.escape_intermediate(perform, b),
            State::CsiEntry => self.csi_entry(perform, b),
            State::CsiParam => self.csi_param(perform, b),
            State::CsiIntermediate => self.csi_intermediate(perform, b),
            State::CsiIgnore => self.csi_ignore(b),
            State::OscString => self.osc(perform, b),
            State::DcsPassthrough => self.dcs(b),
            State::SosPmApcString => self.sos_pm_apc(b),
        }
    }

    // --- Ground / UTF-8 -----------------------------------------------------

    fn ground<P: Perform>(&mut self, perform: &mut P, b: u8) {
        if b < 0x80 {
            perform.print(b as char);
        } else {
            self.utf8_begin(perform, b);
        }
    }

    fn utf8_begin<P: Perform>(&mut self, perform: &mut P, b: u8) {
        // Classic UTF-8 lead-byte dispatch with over-long / range checks.
        if b & 0xE0 == 0xC0 {
            self.utf8_remaining = 1;
            self.utf8_cp = (b as u32 & 0x1F) << 6;
            self.utf8_min = 0x80;
        } else if b & 0xF0 == 0xE0 {
            self.utf8_remaining = 2;
            self.utf8_cp = (b as u32 & 0x0F) << 12;
            self.utf8_min = 0x800;
        } else if b & 0xF8 == 0xF0 {
            self.utf8_remaining = 3;
            self.utf8_cp = (b as u32 & 0x07) << 18;
            self.utf8_min = 0x1_0000;
        } else {
            // Stray continuation or invalid lead byte.
            perform.print('\u{FFFD}');
        }
    }

    fn utf8_continue<P: Perform>(&mut self, perform: &mut P, b: u8) {
        if b & 0xC0 != 0x80 {
            // Not a continuation byte: emit replacement, reprocess `b`.
            self.utf8_remaining = 0;
            perform.print('\u{FFFD}');
            self.step(perform, b);
            return;
        }
        self.utf8_remaining -= 1;
        self.utf8_cp |= (b as u32 & 0x3F) << (6 * self.utf8_remaining as u32);
        if self.utf8_remaining == 0 {
            let cp = self.utf8_cp;
            let valid = cp >= self.utf8_min && cp <= 0x10_FFFF && !(0xD800..=0xDFFF).contains(&cp);
            let c = if valid {
                char::from_u32(cp).unwrap_or('\u{FFFD}')
            } else {
                '\u{FFFD}'
            };
            perform.print(c);
        }
    }

    // --- Escape -------------------------------------------------------------

    fn enter_escape(&mut self) {
        self.state = State::Escape;
        self.params.clear();
        self.intermediates.clear();
        self.ignoring = false;
    }

    fn escape<P: Perform>(&mut self, perform: &mut P, b: u8) {
        match b {
            0x20..=0x2f => {
                self.collect(b);
                self.state = State::EscapeIntermediate;
            }
            0x5b => self.state = State::CsiEntry, // '['
            0x5d => {
                // ']' OSC
                self.osc_raw.clear();
                self.state = State::OscString;
            }
            0x50 => {
                // 'P' DCS
                self.params.clear();
                self.intermediates.clear();
                self.dcs_raw.clear();
                self.dcs_overflow = false;
                self.state = State::DcsPassthrough;
            }
            0x58 | 0x5e | 0x5f => {
                // SOS / PM / APC
                self.state = State::SosPmApcString;
            }
            0x30..=0x4f | 0x51..=0x57 | 0x59 | 0x5a | 0x5c | 0x60..=0x7e => {
                perform.esc_dispatch(&self.intermediates, b);
                self.state = State::Ground;
            }
            _ => self.state = State::Ground,
        }
    }

    fn escape_intermediate<P: Perform>(&mut self, perform: &mut P, b: u8) {
        match b {
            0x20..=0x2f => self.collect(b),
            0x30..=0x7e => {
                perform.esc_dispatch(&self.intermediates, b);
                self.state = State::Ground;
            }
            _ => self.state = State::Ground,
        }
    }

    // --- CSI ----------------------------------------------------------------

    fn csi_entry<P: Perform>(&mut self, perform: &mut P, b: u8) {
        self.params.clear();
        self.intermediates.clear();
        self.ignoring = false;
        self.csi_process(perform, b, true);
    }

    fn csi_param<P: Perform>(&mut self, perform: &mut P, b: u8) {
        self.csi_process(perform, b, false);
    }

    fn csi_process<P: Perform>(&mut self, perform: &mut P, b: u8, entry: bool) {
        match b {
            0x30..=0x39 => {
                self.params.add_digit((b - b'0') as u16);
                self.state = State::CsiParam;
            }
            0x3a => {
                self.params.push_subparam();
                self.state = State::CsiParam;
            }
            0x3b => {
                self.params.push_group();
                self.state = State::CsiParam;
            }
            0x3c..=0x3f
                // Private markers (`< = > ?`) — valid only right after CSI.
                if entry => {
                    self.collect(b);
                    self.state = State::CsiParam;
                }
            0x20..=0x2f => {
                self.collect(b);
                self.state = State::CsiIntermediate;
            }
            0x40..=0x7e => {
                self.params.ensure_first();
                perform.csi_dispatch(
                    &self.params,
                    &self.intermediates,
                    self.ignoring,
                    b as char,
                );
                self.state = State::Ground;
            }
            _ => {
                self.state = State::CsiIgnore;
                self.ignoring = true;
            }
        }
    }

    fn csi_intermediate<P: Perform>(&mut self, perform: &mut P, b: u8) {
        match b {
            0x20..=0x2f => self.collect(b),
            0x40..=0x7e => {
                self.params.ensure_first();
                perform.csi_dispatch(&self.params, &self.intermediates, self.ignoring, b as char);
                self.state = State::Ground;
            }
            0x30..=0x3f => {
                // Parameter byte after an intermediate is illegal.
                self.state = State::CsiIgnore;
            }
            _ => self.state = State::CsiIgnore,
        }
    }

    fn csi_ignore(&mut self, b: u8) {
        if (0x40..=0x7e).contains(&b) {
            self.state = State::Ground;
        }
    }

    fn collect(&mut self, b: u8) {
        if self.intermediates.len() < MAX_INTERMEDIATES {
            self.intermediates.push(b);
        } else {
            self.ignoring = true;
        }
    }

    // --- OSC ----------------------------------------------------------------

    fn osc<P: Perform>(&mut self, perform: &mut P, b: u8) {
        match b {
            0x07 => {
                // BEL terminates OSC.
                self.osc_end(perform, true);
            }
            0x1b => {
                // Handled in `step` (ESC); but ST is ESC \, so we need to peek.
                // `step` already routed ESC here only if not caught earlier;
                // to support ESC \ (ST) we treat a following '\' via Escape.
                // Simplest correct handling: end the OSC now and let ESC start
                // a new sequence; the trailing '\' (if ST) becomes a no-op ESC.
                self.osc_end(perform, false);
                self.enter_escape();
            }
            _ => {
                if self.osc_raw.len() < MAX_OSC_LEN {
                    self.osc_raw.push(b);
                }
            }
        }
    }

    fn osc_end<P: Perform>(&mut self, perform: &mut P, bell: bool) {
        let raw = std::mem::take(&mut self.osc_raw);
        let parts: Vec<&[u8]> = raw.split(|&c| c == b';').collect();
        perform.osc_dispatch(&parts, bell);
        self.state = State::Ground;
    }

    // --- DCS (captured; Sixel decoded by the Perform impl) ------------------

    fn dcs(&mut self, b: u8) {
        // Capture the payload; ST (ESC \) is caught in `step`, ending the state.
        if self.dcs_raw.len() < MAX_DCS {
            self.dcs_raw.push(b);
        } else {
            self.dcs_overflow = true;
        }
    }

    fn dcs_end<P: Perform>(&mut self, perform: &mut P) {
        perform.dcs_dispatch(&self.dcs_raw, self.dcs_overflow);
        self.dcs_raw.clear();
        self.dcs_overflow = false;
    }

    // --- SOS / PM / APC (consumed, not interpreted) -------------------------

    fn sos_pm_apc(&mut self, b: u8) {
        let _ = b;
    }
}
