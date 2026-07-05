//! Kitty graphics protocol (APC `_G…`) behavior, exercised through the public
//! terminal API: transmit, display, chunking, store/put, delete and query.

use ferroterm_core::Terminal;

/// Standard base64 (with padding) — the encoding Kitty uses on the wire.
fn b64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6 & 63) as usize] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// A minimal PNG header enough for the core to detect the format and sniff the
/// dimensions (the core never decodes the pixels — the front-end does).
fn fake_png(w: u32, h: u32) -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]); // bit depth/color type + fake CRC
    v
}

/// Wrap a Kitty command's control + base64 payload in the APC envelope.
fn apc(ctrl: &str, payload: &str) -> Vec<u8> {
    format!("\x1b_G{ctrl};{payload}\x1b\\").into_bytes()
}

fn term() -> Terminal {
    Terminal::new(40, 12, 100)
}

#[test]
fn rgba_transmit_and_display() {
    let mut t = term();
    // A 2x2 RGBA image; distinctive bytes so we can round-trip them.
    let px: Vec<u8> = (0..16).map(|i| (i * 15) as u8).collect();
    t.feed(&apc("a=T,f=32,s=2,v=2", &b64(&px)));

    let ids = t.image_ids();
    assert_eq!(ids.len(), 1, "one image placed");
    let id = ids[0];
    assert_eq!(t.image_size(id), vec![2, 2]);
    assert_eq!(t.image_rgba(id), px, "raw RGBA round-trips 1:1");
    assert!(t.image_mime(id).is_empty(), "raw path has no MIME");
    assert_eq!(t.image_placements().len(), 5, "one placement of 5 ints");
    // Default 8x16 cells → a 2px-tall image occupies one cell row; the cursor
    // lands on the line below.
    assert_eq!(t.cursor(), (0, 1));
}

#[test]
fn rgb_expands_to_rgba_with_opaque_alpha() {
    let mut t = term();
    t.feed(&apc("a=T,f=24,s=1,v=1", &b64(&[10, 20, 30])));
    let id = t.image_ids()[0];
    assert_eq!(t.image_rgba(id), vec![10, 20, 30, 0xff]);
}

#[test]
fn png_is_passed_through_encoded() {
    let mut t = term();
    let png = fake_png(64, 32);
    t.feed(&apc("a=T,f=100", &b64(&png)));
    let id = t.image_ids()[0];
    assert_eq!(
        t.image_encoded(id),
        png,
        "PNG bytes handed to the front-end"
    );
    assert_eq!(t.image_mime(id), "image/png");
    assert!(t.image_rgba(id).is_empty(), "core does not decode the PNG");
}

#[test]
fn chunked_transmission_reassembles() {
    let mut single = term();
    let px: Vec<u8> = (0..64).map(|i| (i * 3) as u8).collect(); // 4x4 RGBA
    single.feed(&apc("a=T,f=32,s=4,v=4", &b64(&px)));
    let want = single.image_rgba(single.image_ids()[0]);

    // Same image split across two APC commands: the first carries the control
    // block + m=1, the continuation carries only m=0 and the rest of the data.
    let full = b64(&px);
    let (a, b) = full.split_at(full.len() / 2);
    let mut chunked = term();
    chunked.feed(&apc("a=T,f=32,s=4,v=4,m=1", a));
    chunked.feed(&apc("m=0", b));

    let ids = chunked.image_ids();
    assert_eq!(ids.len(), 1, "chunked transfer yields exactly one image");
    assert_eq!(chunked.image_rgba(ids[0]), want, "reassembled pixels match");
}

#[test]
fn transmit_then_put_by_id() {
    let mut t = term();
    // a=t stores without displaying.
    t.feed(&apc("a=t,f=32,s=1,v=1,i=42", &b64(&[1, 2, 3, 4])));
    assert!(t.image_ids().is_empty(), "a=t does not display");
    // a=p displays the stored image.
    t.feed(&apc("a=p,i=42", ""));
    let ids = t.image_ids();
    assert_eq!(ids.len(), 1, "a=p places the stored image");
    assert_eq!(t.image_rgba(ids[0]), vec![1, 2, 3, 4]);
}

#[test]
fn delete_clears_kitty_images() {
    let mut t = term();
    t.feed(&apc("a=T,f=32,s=1,v=1", &b64(&[9, 9, 9, 9])));
    t.feed(&apc("a=T,f=32,s=1,v=1", &b64(&[8, 8, 8, 8])));
    assert_eq!(t.image_ids().len(), 2);
    t.feed(&apc("a=d,d=A", ""));
    assert!(
        t.image_ids().is_empty(),
        "a=d clears displayed Kitty images"
    );
}

#[test]
fn query_is_acknowledged() {
    let mut t = term();
    t.feed(&apc("a=q,i=1", ""));
    let out = String::from_utf8_lossy(&t.take_output()).into_owned();
    assert!(out.contains("OK"), "query replies OK, got {out:?}");
    assert!(out.contains("i=1"), "reply echoes the image id");
}

#[test]
fn unsupported_medium_is_refused_cleanly() {
    let mut t = term();
    // t=f (file transmission) must not read anything or place an image.
    t.feed(&apc("a=T,t=f,f=100", &b64(b"/etc/passwd")));
    assert!(t.image_ids().is_empty(), "file-medium transfer is refused");
    // The terminal keeps working afterward.
    t.feed(b"ok");
    assert_eq!(t.cell_char(0, 0), 'o');
}

#[test]
fn transmit_success_acknowledges() {
    let mut t = term();
    t.feed(&apc("a=T,f=32,s=1,v=1,i=5", &b64(&[0, 0, 0, 0])));
    let out = String::from_utf8_lossy(&t.take_output()).into_owned();
    assert!(
        out.contains("OK") && out.contains("i=5"),
        "transmit acks, got {out:?}"
    );
}
