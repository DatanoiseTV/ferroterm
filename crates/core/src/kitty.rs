//! Kitty graphics protocol (APC `_G…` sequences).
//!
//! Supported: direct transmission (`t=d`, the default) of RGB (`f=24`),
//! RGBA (`f=32`) and PNG (`f=100`) images, chunked over multiple APC commands
//! (`m=1`), display at the cursor via `a=T` (transmit + display) or `a=p`
//! (display a previously transmitted image), delete (`a=d`) and query (`a=q`).
//! Base64 is the only accepted payload encoding.
//!
//! Deliberately unsupported (and refused so the stream stays in sync): file /
//! temp-file / shared-memory transmission (`t=f|t|s`) — honoring it would let a
//! terminal escape read arbitrary host files; zlib payload compression (`o=z`)
//! — the core carries no inflate; animation frames, Unicode placeholders and
//! relative / z-index placement.
//!
//! This module is pure parsing; chunk accumulation, storage and placement live
//! in [`crate::terminal`], which has the grid geometry needed to lay an image
//! out in cells.

use std::str;

/// A parsed Kitty control block (the `key=value,…` list before the `;`).
///
/// Fields carry the protocol defaults for the keys this core acts on; keys it
/// doesn't understand are ignored.
#[derive(Clone)]
pub struct Cmd {
    /// `a`: action — `t` transmit, `T` transmit + display, `p` put/display a
    /// stored image, `d` delete, `q` query. Default `t`.
    pub action: u8,
    /// `f`: pixel format — `24` RGB, `32` RGBA, `100` PNG. Default `32`.
    pub format: u32,
    /// `i`: client image id (0 = none), used to reference a stored image later.
    pub id: u32,
    /// `s` / `v`: source width / height in pixels (raw RGB/RGBA formats).
    pub width: u32,
    pub height: u32,
    /// `c` / `r`: requested display size in cells (0 = derive from pixels).
    pub cols: u32,
    pub rows: u32,
    /// `m`: more chunks follow (`m=1`).
    pub more: bool,
    /// `t`: transmission medium — `d` direct base64 (the only one honored).
    pub medium: u8,
    /// `o=z`: payload is zlib-compressed (refused; no inflate in-core).
    pub compressed: bool,
    /// `d`: delete selector for `a=d` — `a`/`A` all, `i`/`I` by id.
    pub delete: u8,
    /// `q`: quietness — 0 verbose, 1 suppress OK, 2 suppress all replies.
    pub quiet: u32,
}

impl Default for Cmd {
    fn default() -> Self {
        Cmd {
            action: b't',
            format: 32,
            id: 0,
            width: 0,
            height: 0,
            cols: 0,
            rows: 0,
            more: false,
            medium: b'd',
            compressed: false,
            delete: b'a',
            quiet: 0,
        }
    }
}

/// Parse one Kitty APC command. `payload` is the bytes between `ESC _` and the
/// `ESC \` terminator (i.e. starting with `G`). Returns the control block and
/// the raw base64 data slice (possibly empty), or `None` if this APC is not a
/// `G` (graphics) command.
pub fn parse(payload: &[u8]) -> Option<(Cmd, &[u8])> {
    let rest = payload.strip_prefix(b"G")?;
    let (ctrl, data) = match rest.iter().position(|&c| c == b';') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, &[][..]),
    };
    let mut cmd = Cmd::default();
    for kv in ctrl.split(|&c| c == b',') {
        if kv.is_empty() {
            continue;
        }
        let Ok(kv) = str::from_utf8(kv) else {
            continue;
        };
        let Some((k, v)) = kv.split_once('=') else {
            continue;
        };
        match k {
            "a" => cmd.action = v.bytes().next().unwrap_or(b't'),
            "f" => cmd.format = v.parse().unwrap_or(32),
            "i" => cmd.id = v.parse().unwrap_or(0),
            "s" => cmd.width = v.parse().unwrap_or(0),
            "v" => cmd.height = v.parse().unwrap_or(0),
            "c" => cmd.cols = v.parse().unwrap_or(0),
            "r" => cmd.rows = v.parse().unwrap_or(0),
            "m" => cmd.more = v == "1",
            "t" => cmd.medium = v.bytes().next().unwrap_or(b'd'),
            "o" => cmd.compressed = v == "z",
            "d" => cmd.delete = v.bytes().next().unwrap_or(b'a'),
            "q" => cmd.quiet = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    Some((cmd, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_empty() {
        let (c, data) = parse(b"G;").unwrap();
        assert_eq!(c.action, b't');
        assert_eq!(c.format, 32);
        assert_eq!(c.medium, b'd');
        assert!(data.is_empty());
    }

    #[test]
    fn not_a_graphics_command() {
        assert!(parse(b"Xhello").is_none());
        assert!(parse(b"").is_none());
    }

    #[test]
    fn full_control_block() {
        let (c, data) = parse(b"Ga=T,f=24,i=7,s=4,v=3,c=10,r=5,m=1,q=1;AAAA").unwrap();
        assert_eq!(c.action, b'T');
        assert_eq!(c.format, 24);
        assert_eq!(c.id, 7);
        assert_eq!(c.width, 4);
        assert_eq!(c.height, 3);
        assert_eq!(c.cols, 10);
        assert_eq!(c.rows, 5);
        assert!(c.more);
        assert_eq!(c.quiet, 1);
        assert_eq!(data, b"AAAA");
    }

    #[test]
    fn unsupported_medium_and_compression_flagged() {
        let (c, _) = parse(b"Ga=T,t=f,o=z;").unwrap();
        assert_eq!(c.medium, b'f');
        assert!(c.compressed);
    }

    #[test]
    fn malformed_pairs_are_skipped() {
        // Bare keys, empty pairs and unknown keys must not derail parsing.
        let (c, _) = parse(b"Ga=q,,zz,x=1,i=9;").unwrap();
        assert_eq!(c.action, b'q');
        assert_eq!(c.id, 9);
    }
}
