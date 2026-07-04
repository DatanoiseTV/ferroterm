//! Character display width (East-Asian-Width approximation).
//!
//! Returns `0` for zero-width combining marks / joiners, `2` for wide
//! (CJK, fullwidth, most emoji) and `1` otherwise. The ranges below cover the
//! common cases; this is intentionally a compact table rather than the full
//! Unicode database.

/// Display columns occupied by `c`.
pub fn char_width(c: char) -> u8 {
    let u = c as u32;
    if u == 0 {
        return 0;
    }
    if u < 0x20 || (0x7f..0xa0).contains(&u) {
        // C0/C1 controls are not printed; callers handle them separately.
        return 1;
    }
    if is_zero_width(u) {
        return 0;
    }
    if is_wide(u) {
        return 2;
    }
    1
}

fn is_zero_width(u: u32) -> bool {
    matches!(u,
        0x0300..=0x036F | // combining diacritical marks
        0x0483..=0x0489 |
        0x0591..=0x05BD |
        0x0610..=0x061A |
        0x064B..=0x065F |
        0x0670..=0x0670 |
        0x06D6..=0x06DC |
        0x0E31..=0x0E31 |
        0x0E34..=0x0E3A |
        0x1AB0..=0x1AFF | // combining diacritical marks extended
        0x1DC0..=0x1DFF | // combining diacritical marks supplement
        0x200B..=0x200F | // zero-width space / joiners / marks
        0x202A..=0x202E |
        0x2060..=0x2064 |
        0x20D0..=0x20FF | // combining marks for symbols
        0xFE00..=0xFE0F | // variation selectors
        0xFE20..=0xFE2F | // combining half marks
        0xE0100..=0xE01EF // variation selectors supplement
    )
}

fn is_wide(u: u32) -> bool {
    matches!(u,
        0x1100..=0x115F | // Hangul Jamo
        0x2329..=0x232A |
        0x2E80..=0x303E | // CJK radicals, Kangxi
        0x3041..=0x33FF | // Hiragana .. CJK compat
        0x3400..=0x4DBF | // CJK ext A
        0x4E00..=0x9FFF | // CJK unified
        0xA000..=0xA4CF | // Yi
        0xAC00..=0xD7A3 | // Hangul syllables
        0xF900..=0xFAFF | // CJK compat ideographs
        0xFE10..=0xFE19 | // vertical forms
        0xFE30..=0xFE6F | // CJK compat forms / small forms
        0xFF00..=0xFF60 | // fullwidth forms
        0xFFE0..=0xFFE6 | // fullwidth signs
        0x1F300..=0x1F64F | // emoji: symbols & pictographs, emoticons
        0x1F900..=0x1F9FF | // supplemental symbols & pictographs
        0x1FA70..=0x1FAFF | // symbols & pictographs extended-A
        0x20000..=0x3FFFD // CJK ext B..
    )
}
