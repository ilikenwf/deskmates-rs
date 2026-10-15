//! Pure-Rust FAZ scene container parser + decoder, a direct port of
//! `analysis/faz.py` and the frame/script logic of `analysis/faz_convert.py`.
//!
//! Layout: name\0 | DIB templates (40-byte header + palette) | script
//! (NUL-separated name/value string pairs) | frame table | frame pixel
//! blocks (bottom-up, stride-padded rows).
//!
//! Frame table forms:
//! * old: u16 ids terminated by 0xffff (Tahni/FatB installers);
//! * count: u32 count + count u16 ids + count flag bytes, detected by exact
//!   size validation in a window around the script end, then a full-file scan
//!   (the `analysis/maeka.py` fallback).

use image::RgbaImage;
use std::collections::HashMap;

const MAXSTR: usize = 4000;
const DIB_MAGIC: [u8; 4] = [0x28, 0x00, 0x00, 0x00];

/// Click-hotspot map: one byte per pixel (bottom-up, `width` wide) indexing
/// `colors`; the color under the cursor names the touch sequence
/// (`T` + python-style hex + state) defined in the same file.
#[derive(Clone, Debug)]
pub struct Touch {
    pub colors: Vec<u32>,
    pub width: u32,
    pub height: u32,
    pub bitmap: Vec<u8>,
}

/// One parsed FAZ container (the subset the engine needs).
pub struct Faz {
    pub name: String,
    /// DIB template palettes (u32 0xRRGGBB entries, as `faz.py` interprets them).
    pub dibs: Vec<Vec<u32>>,
    /// Script string pairs (latin-1 decoded).
    pub pairs: Vec<String>,
    pub frames: HashMap<u16, Vec<u8>>,
    pub dims: Option<(u32, u32)>,
    pub bpp: Option<u8>,
    /// True when the frame table used the new count-prefixed form.
    pub count_table: bool,
    pub frame_flags: HashMap<u16, u8>,
    /// Hotspot bitmap, present on touch.FAS containers.
    pub touch: Option<Touch>,
}

fn le16(d: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(i..i + 2)?.try_into().unwrap()))
}

fn le32(d: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(i..i + 4)?.try_into().unwrap()))
}

fn le_i32(d: &[u8], i: usize) -> Option<i32> {
    Some(i32::from_le_bytes(d.get(i..i + 4)?.try_into().unwrap()))
}

fn find_nul(d: &[u8], from: usize) -> Option<usize> {
    d[from..].iter().position(|&c| c == 0).map(|p| p + from)
}

/// `faz._printable`: tabs/CR/LF and everything from space up.
fn printable(b: &[u8]) -> bool {
    b.iter().all(|&c| (9..=13).contains(&c) || c >= 32)
}

/// latin-1 decode (what Python's `.decode('latin1')` produces).
fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| char::from(c)).collect()
}

/// Bytes per pixel block: rows padded to a 4-byte stride.
pub fn block_size(w: u32, h: u32, bpp: u32) -> usize {
    let row = ((w as usize * bpp as usize / 8) + 3) & !3;
    row * h as usize
}

/// Try the count-prefixed table at offset `c`; returns (ids, flags, data_start).
fn try_count_table(d: &[u8], c: usize, bs: usize) -> Option<(Vec<u16>, Vec<u8>, usize)> {
    let v = le32(d, c)?;
    if v > 4000 {
        return None;
    }
    if d.len().wrapping_sub(c + 4) != (v as usize).wrapping_mul(bs + 3) {
        return None;
    }
    let v = v as usize;
    let mut ids = Vec::with_capacity(v);
    for k in 0..v {
        ids.push(le16(d, c + 4 + 2 * k)?);
    }
    let flags: Vec<u8> = d.get(c + 4 + 2 * v..c + 4 + 3 * v)?.to_vec();
    Some((ids, flags, c + 4 + 3 * v))
}

/// The first `T0x<hex><state>\0` touch-sequence name in `d`, backed up to the
/// start of its NUL-terminated token. touch.FAS seq sections begin with these;
/// the generic printable-string heuristic fires inside the hotspot bitmap.
fn find_touch_seq_start(d: &[u8], from: usize) -> Option<usize> {
    const STATES: [&[u8]; 5] = [b"up", b"down", b"start", b"loop", b"stop"];
    let mut p = from;
    while p + 4 < d.len() {
        if d[p] == b'T' && d.get(p + 1..p + 3) == Some(&b"0x"[..]) {
            let mut q = p + 3;
            while q < d.len() && d[q].is_ascii_hexdigit() {
                q += 1;
            }
            for st in STATES.iter() {
                if d.get(q..q + st.len()) == Some(*st) && d.get(q + st.len()) == Some(&0) {
                    let mut s = p;
                    while s > from && d[s - 1] != 0 {
                        s -= 1;
                    }
                    return Some(s);
                }
            }
        }
        p += 1;
    }
    None
}

/// Locate the hotspot section (color table + bottom-up 8bpp bitmap) that sits
/// between the DIB templates and the seq section. Validated against the hex
/// colors actually referenced by the parsed `T0x…` sequence names.
fn find_touch_section(d: &[u8], from: usize, to: usize, height: u32, pairs: &[String]) -> Option<Touch> {
    let mut want: Vec<u32> = Vec::new();
    for p in pairs {
        let Some(rest) = p
            .strip_prefix("T0x")
            .or_else(|| p.strip_prefix("Demo_T0x"))
        else {
            continue;
        };
        let hex: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        if !hex.is_empty() {
            if let Ok(v) = u32::from_str_radix(&hex, 16) {
                want.push(v);
            }
        }
    }
    if want.is_empty() {
        return None;
    }
    for p in from..to.saturating_sub(6) {
        let tc = le16(d, p)? as usize;
        if tc == 0 || tc > 256 {
            continue;
        }
        let tw = le16(d, p + 2 + tc * 4)? as usize;
        if tw == 0 || tw > 1024 {
            continue;
        }
        let bmp = p + 4 + tc * 4;
        let need = bmp.checked_add(tw.checked_mul(height as usize)?)?;
        if need > d.len() {
            continue;
        }
        let colors: Vec<u32> = (0..tc).map(|k| le32(d, p + 2 + k * 4).unwrap_or(0)).collect();
        let bitmap = &d[bmp..need];
        if bitmap.iter().take(4096).any(|&b| b as usize >= tc) {
            continue;
        }
        let ok = want.iter().any(|w| {
            colors
                .iter()
                .position(|c| c == w)
                .is_some_and(|ix| bitmap.contains(&(ix as u8)))
        });
        if ok {
            return Some(Touch {
                colors,
                width: tw as u32,
                height,
                bitmap: bitmap.to_vec(),
            });
        }
    }
    None
}

/// Parse FAZ bytes; `None` if the file is not a parseable container
/// (mirrors the exceptions `faz.parse_faz` raises, which callers skip).
pub fn parse_faz(d: &[u8]) -> Option<Faz> {
    let z = find_nul(d, 0)?;
    let name = latin1(&d[..z]);

    // DIB templates
    let mut i = z + 1;
    while i < 2000 && d.get(i..i + 4) != Some(&DIB_MAGIC) {
        i += 1;
    }
    let mut dibs: Vec<Vec<u32>> = Vec::new();
    let mut dims: Option<(u32, u32)> = None;
    let mut bpp: Option<u8> = None;
    while i + 40 <= d.len() && d.get(i..i + 4) == Some(&DIB_MAGIC) {
        let w = le_i32(d, i + 4)?;
        let h = le_i32(d, i + 8)?;
        let bppu = le16(d, i + 14)?;
        if (bppu != 4 && bppu != 8) || !(0 < w && w <= 1024 && 0 < h && h <= 1024) {
            break;
        }
        let ncol: usize = if bppu == 4 { 16 } else { 256 };
        let mut pal = Vec::with_capacity(ncol);
        for k in 0..ncol {
            pal.push(le32(d, i + 40 + 4 * k)?);
        }
        dibs.push(pal);
        dims = Some((w as u32, h as u32));
        bpp = Some(bppu as u8);
        i += 40 + ncol * 4;
    }

    // script: first position with >=3 consecutive NUL-term printable strings.
    // touch.FAS seq sections start at the first `T0x…` name (the generic
    // heuristic would land inside the hotspot bitmap).
    let mut s = find_touch_seq_start(d, i).filter(|p| *p > i).unwrap_or(i);
    let mut found = false;
    while s < d.len().saturating_sub(4) && !found {
        let mut p = s;
        let mut ok = true;
        for _ in 0..3 {
            let e = match find_nul(d, p) {
                Some(e) => e,
                None => {
                    ok = false;
                    break;
                }
            };
            if e.wrapping_sub(p) < 1 || e - p > MAXSTR || !printable(&d[p..e]) {
                ok = false;
                break;
            }
            p = e + 1;
        }
        if ok {
            found = true;
            break;
        }
        s += 1;
    }
    let _ = found;
    let mut pairs: Vec<String> = Vec::new();
    while s < d.len() {
        let e = match find_nul(d, s) {
            Some(e) => e,
            None => break,
        };
        if e - s > MAXSTR {
            break;
        }
        let t = &d[s..e];
        if t.is_empty() || !printable(t) {
            break;
        }
        pairs.push(latin1(t));
        s = e + 1;
    }

    // Hotspot bitmap (touch.FAS): sits between the DIBs and the seq section.
    let touch = if pairs.iter().any(|p| p.starts_with("T0x") || p.starts_with("Demo_T0x")) {
        find_touch_section(d, i, s, dims.map(|(_, h)| h).unwrap_or(200), &pairs)
    } else {
        None
    };

    // frame table: count form (window around script end, then full-file
    // scan per maeka.py), else the old 0xffff-terminated scan.
    let bs = match dims {
        Some((w, h)) => block_size(w, h, bpp.unwrap_or(4) as u32),
        None => 20000,
    };
    let mut ids: Vec<u16> = Vec::new();
    let mut flags: Option<Vec<u8>> = None;
    let mut count_table = false;
    let mut data_start: Option<usize> = None;
    // Old format FIRST: [u16 count @ s-2][u16 ids incl. leading 0 @ s]
    // [0xffff][n flag bytes][n blocks]. The count-form equation below
    // false-positives on these files — the u32 at s-2 (count byte + the
    // three NULs of the leading id 0) satisfies len-(c+4)==v*(bs+3) —
    // and then drops the leading 0 id, shifting every frame one pose
    // and mapping the last block to bogus id 65535. Verify the old
    // format's own invariants (count matches id run, exact trailing
    // length) and trust them over the count scan.
    if s >= 2 {
        let mut old_ids: Vec<u16> = Vec::new();
        let mut p = s;
        let mut term: Option<usize> = None;
        while p + 2 <= d.len() {
            match le16(d, p) {
                Some(0xffff) => {
                    term = Some(p);
                    break;
                }
                Some(v) => old_ids.push(v),
                None => break,
            }
            p += 2;
        }
        if let Some(t) = term {
            let n = old_ids.len();
            let after = t + 2;
            if le16(d, s - 2) == Some(n as u16) && d.len() == after + n + n * bs {
                ids = old_ids;
                flags = Some(d[after..after + n].to_vec());
                data_start = Some(after + n);
            }
        }
    }
    if data_start.is_none() {
        let hi = (s + 16).min(d.len().saturating_sub(4));
        for c in (s.saturating_sub(8)).max(0)..hi {
            if let Some((ids2, flags2, ds)) = try_count_table(d, c, bs) {
                ids = ids2;
                flags = Some(flags2);
                count_table = true;
                data_start = Some(ds);
                break;
            }
        }
        if data_start.is_none() {
            let hi = d.len().saturating_sub(4);
            for c in 0..hi {
                if let Some((ids2, flags2, ds)) = try_count_table(d, c, bs) {
                    ids = ids2;
                    flags = Some(flags2);
                    count_table = true;
                    data_start = Some(ds);
                    break;
                }
            }
        }
    }
    let data: &[u8] = match data_start {
        Some(ds) => &d[ds..],
        None => {
            let mut i2 = s;
            while i2 + 2 <= d.len() {
                let v = le16(d, i2)?;
                if v == 0xffff {
                    break;
                }
                ids.push(v);
                i2 += 2;
            }
            d.get(i2 + ids.len()..).unwrap_or(&[])
        }
    };

    let nblocks = data.len() / bs;
    let mut frames = HashMap::new();
    let mut frame_flags = HashMap::new();
    for k in 0..nblocks.min(ids.len()) {
        let blk = data[k * bs..(k + 1) * bs].to_vec();
        let id = ids[k];
        if let Some(f) = &flags {
            frame_flags.insert(id, f[k]);
        }
        frames.insert(id, blk);
    }

    Some(Faz { name, dibs, pairs, frames, dims, bpp, count_table, frame_flags, touch })
}

// ---------------------------------------------------------------------------
// Script parsing (port of faz_convert.py main() def/entry extraction)
// ---------------------------------------------------------------------------
/// One token of a parsed def body; mirrors the dicts `parse_reflist` emits
/// (frame tokens carry id/x/y, name tokens carry name; rep defaults to 1).
pub enum Tok {
    Frame { id: u32, x: i32, y: i32, rep: u32 },
    Name { name: String, rep: u32 },
    /// ` !name` — play a sound from the character's wave archive.
    Sound { name: String },
}

fn is_digit(c: u8) -> bool {
    c.is_ascii_digit()
}

fn read_digits(d: &[u8], mut p: usize, max: usize) -> (u32, usize) {
    let mut v: u32 = 0;
    let mut n = 0;
    while n < max && p < d.len() && is_digit(d[p]) {
        v = v * 10 + (d[p] - b'0') as u32;
        p += 1;
        n += 1;
    }
    (v, n)
}

fn ident_at(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic() || c >= 0x80
}

fn ident_cont(c: u8) -> bool {
    ident_at(c) || is_digit(c)
}
/// `str.isidentifier` approximation for latin-1 script strings.
fn is_identifier(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && ident_at(b[0]) && b[1..].iter().all(|&c| ident_cont(c))
}


/// Port of `faz_convert.parse_reflist`: scan a def body for frame/name refs.
pub fn parse_reflist(s: &[u8]) -> Vec<Tok> {
    let mut toks = Vec::new();
    let (mut offx, mut offy) = (0i32, 0i32);
    let mut p = 0usize;
    while p < s.len() {
        let c = s[p];
        // ` !name`: sound-effect token (original REGEX_SOUND ` !([^,]+)`),
        // terminated by the next comma or group close.
        if c == b' ' && s.get(p + 1) == Some(&b'!') {
            let mut q = p + 2;
            while q < s.len() && s[q] != b',' && s[q] != b')' {
                q += 1;
            }
            let name = latin1(&s[p + 2..q]).trim_end().to_string();
            if !name.is_empty() {
                toks.push(Tok::Sound { name });
            }
            p = q;
            continue;
        }
        // \[(-?\d+),(-?\d+)\]
        if c == b'[' {
            let mut q = p + 1;
            let negx = s.get(q) == Some(&b'-');
            if negx {
                q += 1;
            }
            let (xv, xn) = read_digits(s, q, usize::MAX);
            if xn > 0 && s.get(q + xn) == Some(&b',') {
                let mut q2 = q + xn + 1;
                let negy = s.get(q2) == Some(&b'-');
                if negy {
                    q2 += 1;
                }
                let (yv, yn) = read_digits(s, q2, usize::MAX);
                if yn > 0 && s.get(q2 + yn) == Some(&b']') {
                    offx = if negx { -(xv as i32) } else { xv as i32 };
                    offy = if negy { -(yv as i32) } else { yv as i32 };
                    p = q2 + yn + 1;
                    continue;
                }
            }
            p += 1;
            continue;
        }
        // (\d{4,5})(?:-(\d{4,5}))?(?:\*(\d+))?
        if is_digit(c) {
            let (a, n) = read_digits(s, p, 5);
            if n >= 4 {
                let mut q = p + n;
                let mut b = a;
                if s.get(q) == Some(&b'-') {
                    let (bv, bn) = read_digits(s, q + 1, 5);
                    if bn >= 4 {
                        b = bv;
                        q += 1 + bn;
                    }
                }
                let mut rep = 1u32;
                if s.get(q) == Some(&b'*') {
                    let (rv, rn) = read_digits(s, q + 1, usize::MAX);
                    if rn > 0 {
                        rep = rv;
                        q += 1 + rn;
                    }
                }
                let step: i64 = if b >= a { 1 } else { -1 };
                let mut i = a as i64;
                let mut first = true;
                loop {
                    toks.push(Tok::Frame {
                        id: i as u32,
                        x: offx,
                        y: offy,
                        rep: if first { rep } else { 1 },
                    });
                    first = false;
                    if i == b as i64 {
                        break;
                    }
                    i += step;
                }
                offx = 0;
                offy = 0;
                p = q;
                continue;
            }
            // (\d) single digit: matched and ignored by parse_reflist
            p += 1;
            continue;
        }
        // ([a-zA-Z_][a-zA-Z0-9_]*)\*(\d+) | ([a-zA-Z_][a-zA-Z0-9_]*)
        // ASCII-only classes, exactly like the Python REF regex.
        if c == b'_' || c.is_ascii_alphabetic() {
            let mut q = p + 1;
            while q < s.len() && (s[q] == b'_' || s[q].is_ascii_alphanumeric()) {
                q += 1;
            }
            let nm = &s[p..q];
            if s.get(q) == Some(&b'*') {
                let (rv, rn) = read_digits(s, q + 1, usize::MAX);
                if rn > 0 {
                    toks.push(Tok::Name { name: latin1(nm), rep: rv });
                    p = q + 1 + rn;
                    continue;
                }
            }
            // group(8) branch: 'pause' -> name token; leading digit impossible
            // for [a-zA-Z_] starts; else name token.
            toks.push(Tok::Name { name: latin1(nm), rep: 1 });
            p = q;
            continue;
        }
        // \* bare star: ignored
        p += 1;
    }
    toks
}

const ENTRY_KEYS: [&str; 6] =
    ["all", "all_demo", "all_slow", "all_slow_demo", "all_fast", "all_fast_demo"];
const ENTRY_ORDER: [&str; 6] =
    ["all", "all_slow", "all_fast", "all_demo", "all_slow_demo", "all_fast_demo"];

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// `^\{([\w%|]+)\}(?:,pause)?$` -> choice entry list.
fn match_brace_entry(b: &str) -> Option<Vec<String>> {
    let s = b.as_bytes();
    if s.first() != Some(&b'{') {
        return None;
    }
    let end = s.iter().rposition(|&c| c == b'}')?;
    if end < 1 {
        return None;
    }
    if !s[1..end].iter().all(|&c| is_word(c) || c == b'%' || c == b'|') {
        return None;
    }
    let rest = &s[end + 1..];
    if !rest.is_empty() && rest != b",pause" {
        return None;
    }
    let inner = &s[1..end];
    Some(
        inner
            .split(|&c| c == b'|')
            .map(|x| latin1(x.split(|&c| c == b'%').next().unwrap_or(x)))
            .collect(),
    )
}

/// `^(\w+)(?:%\d+)?(?:,pause)?$` -> single entry name.
fn match_plain_entry(b: &str) -> Option<Vec<String>> {
    let s = b.as_bytes();
    let mut n = 0;
    while n < s.len() && is_word(s[n]) {
        n += 1;
    }
    if n == 0 {
        return None;
    }
    let mut q = n;
    if s.get(q) == Some(&b'%') {
        let (dv, dn) = read_digits(s, q + 1, usize::MAX);
        let _ = dv;
        if dn == 0 {
            return None;
        }
        q += 1 + dn;
    }
    if q < s.len() && &s[q..] != b",pause" {
        return None;
    }
    Some(vec![latin1(&s[..n])])
}

/// Extract (entries, defs-body list) from script pairs, mirroring
/// faz_convert.main(). Returns defs as (key, body) in pair order.
pub fn script_defs(f: &Faz) -> (Vec<String>, Vec<(String, Vec<u8>)>) {
    let pairs = &f.pairs;
    let mut defs: Vec<(String, Vec<u8>)> = Vec::new();
    let mut i = 0usize;
    while i + 1 < pairs.len() {
        let a = &pairs[i];
        let b = &pairs[i + 1];
        if ENTRY_KEYS.contains(&a.as_str()) {
            i += 2;
            continue;
        }
        if b.starts_with('(') || b.starts_with('{') || (is_identifier(a) && !b.is_empty()) {
            defs.push((a.clone(), b.clone().into_bytes()));
            i += 2;
        } else {
            i += 1;
        }
    }
    let mut entry_names: Vec<String> = Vec::new();
    for key in ENTRY_ORDER {
        let mut k = 0usize;
        while k + 1 < pairs.len() {
            if pairs[k] == key {
                let b = &pairs[k + 1];
                entry_names = match_brace_entry(b)
                    .or_else(|| match_plain_entry(b))
                    .unwrap_or_default();
                break;
            }
            k += 2;
        }
        if !entry_names.is_empty() {
            break;
        }
    }
    if entry_names.is_empty() {
        // Python iterates the defs dict: unique keys, first-insertion order.
        let mut seen: Vec<String> = Vec::new();
        for (k, _) in &defs {
            if (k == &f.name || k.starts_with(&f.name)) && !seen.contains(k) {
                seen.push(k.clone());
            }
        }
        entry_names = seen;
    }
    (entry_names, defs)
}

/// faz_convert.main() scene filter: has frames or pairs, has a DIB palette,
/// ASCII `^[A-Za-z]\w*$` name, not a Tahni3D float scene.
pub fn scene_ok(f: &Faz) -> bool {
    if f.frames.is_empty() && f.pairs.is_empty() {
        return false;
    }
    if f.dibs.is_empty() {
        return false;
    }
    let n = f.name.as_bytes();
    let ok = !n.is_empty()
        && n.iter().all(|c| c.is_ascii())
        && (n[0].is_ascii_alphabetic())
        && n[1..].iter().all(|&c| c.is_ascii_alphanumeric() || c == b'_');
    ok && !f.name.starts_with("Tahni3D")
}

/// Demo placeholder detection: the original demo shipped locked scenes as
/// the identical "?" question-mark bitmap. A scene is a placeholder iff it
/// has at most one frame, or all frame blocks are byte-identical. Such
/// scenes are not playable and get no entry points.
pub fn is_placeholder(f: &Faz) -> bool {
    if f.frames.len() <= 1 {
        return true;
    }
    let mut it = f.frames.values();
    match it.next() {
        Some(first) => it.all(|b| b == first),
        None => true,
    }
}

// ---------------------------------------------------------------------------
// Pixel decode
// ---------------------------------------------------------------------------

/// Decode one frame to RGBA: bottom-up rows flipped, palette applied
/// (stored u32 = 0xRRGGBB, matching faz_convert's PLTE writer), alpha 0 at
/// the transparent key index.
pub fn decode_frame(f: &Faz, id: u16) -> Option<RgbaImage> {
    let blk = f.frames.get(&id)?;
    let (w, h) = f.dims?;
    let bpp = f.bpp? as u32;
    let stride = ((w as usize * bpp as usize / 8) + 3) & !3;
    if blk.len() < stride * h as usize {
        return None;
    }
    // Per-frame palette: dibs[2*flag] (odd DIBs are tint masks);
    // transparent key = last black (>=1) index, else 8. Every table
    // format carries per-frame flag bytes and per-frame DIB palettes:
    // kahli's frame 0 is encoded against dib2 (background black at
    // index 10, gray at 8), oska's background black is index 9. Keying
    // on index 8 unconditionally — or decoding flagged frames against
    // dib0 — painted an opaque black box behind those characters.
    let pal: &Vec<u32>;
    let key: Option<usize>;
    match f.frame_flags.get(&id) {
        Some(&flag) => {
            pal = f.dibs.get(usize::from(flag).saturating_mul(2).min(f.dibs.len() - 1))?;
            let blacks: Vec<usize> =
                (1..pal.len()).filter(|&j| (pal[j] & 0xFFFFFF) == 0).collect();
            key = Some(blacks.last().copied().unwrap_or(8));
        }
        None => {
            pal = f.dibs.first()?;
            key = None;
        }
    }
    let mut img = RgbaImage::new(w, h);
    for (y, row) in img.enumerate_rows_mut() {
        // bottom-up: top row y comes from source row h-1-y
        let src = &blk[(h as usize - 1 - y as usize) * stride..];
        for (x, _, px) in row {
            let idx = if bpp == 8 {
                src[x as usize] as usize
            } else {
                let b = src[(x / 2) as usize];
                if x % 2 == 0 { (b >> 4) as usize } else { (b & 15) as usize }
            };
            let p = *pal.get(idx)?;
            let transparent = match key {
                Some(k) => idx == k,
                None => idx == 8 || (p & 0xFFFFFF) == 0x00C0FF,
            };
            *px = image::Rgba([
                (p >> 16) as u8,
                (p >> 8) as u8,
                p as u8,
                if transparent { 0 } else { 255 },
            ]);
        }
    }
    Some(img)
}

/// Audit tool: labeled contact sheets of every scene (4 sampled frames each)
/// for visual content classification. Run: `cargo test sheet_dump -- --ignored --nocapture`
#[cfg(test)]
mod sheet_tests {
    use super::*;
    use ab_glyph::{Font as _, FontArc, PxScale, ScaleFont};
    use image::ImageBuffer;

    fn put_text(img: &mut RgbaImage, font: &FontArc, text: &str, x: u32, y: u32, sz: f32) {
        let scale = PxScale { x: sz, y: sz };
        let sf = font.as_scaled(scale);
        let mut pen = x as f32;
        for g in text.chars() {
            let gid = sf.glyph_id(g);
            let glyph = gid.with_scale_and_position(scale, ab_glyph::point(pen, y as f32));
            if let Some(v) = font.outline_glyph(glyph) {
                let b = v.px_bounds();
                v.draw(|gx, gy, cov| {
                    let px = b.min.x as i32 + gx as i32;
                    let py = b.min.y as i32 + gy as i32;
                    if cov > 0.4 && px >= 0 && py >= 0 && (px as u32) < img.width() && (py as u32) < img.height() {
                        img.put_pixel(px as u32, py as u32, image::Rgba([255, 255, 0, 255]));
                    }
                });
            }
            pen += sf.h_advance(gid);
        }
    }

    #[test]
    #[ignore]
    fn sheet_dump() {
        let root = format!("{}/assets/characters", env!("CARGO_MANIFEST_DIR"));
        let font = FontArc::try_from_vec(
            std::fs::read(format!("{root}/../fonts/LiberationSans-Regular.ttf")).unwrap(),
        )
        .unwrap();
        for ch in ["tahni", "maeka", "kahli", "fatb"] {
            let dir = format!("{root}/{ch}/scenes");
            let mut files: Vec<_> = std::fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().path())
                .filter(|p| p.extension().is_none() || p.extension().unwrap() == "gz")
                .collect();
            files.sort();
            let (tw, th) = (140u32, 200u32); // tile incl. label
            let cols = 6usize;
            let per_sheet = 24;
            let mut sheet: Option<ImageBuffer<image::Rgba<u8>, Vec<u8>>> = None;
            for (i, path) in files.iter().enumerate() {
                let name = path.file_name().unwrap().to_string_lossy().replace(".bin.gz", "");
                let gz = std::fs::File::open(path).unwrap();
                let mut raw = Vec::new();
                std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(gz), &mut raw).unwrap();
                let Some(f) = parse_faz(&raw) else { continue };
                let mut ids: Vec<u16> = f.frames.keys().copied().collect();
                ids.sort();
                if ids.is_empty() {
                    continue;
                }
                let slot = i % per_sheet;
                if slot == 0 {
                    if let Some(s) = sheet.take() {
                        s.save(format!("/tmp/sheets/{ch}_{}.png", i / per_sheet)).unwrap();
                    }
                    sheet = Some(ImageBuffer::from_pixel(cols as u32 * tw, (per_sheet / cols) as u32 * th, image::Rgba([24, 24, 24, 255])));
                }
                let img = sheet.as_mut().unwrap();
                let (cx, cy) = ((slot % cols) as u32 * tw, (slot / cols) as u32 * th);
                put_text(img, &font, &name, cx + 2, cy + 2, 11.0);
                // 4 sampled frames across the scene timeline
                for (k, frac) in [0.25f32, 0.5, 0.75, 0.95].iter().enumerate() {
                    let id = ids[((ids.len() as f32 * *frac) as usize).min(ids.len() - 1)];
                    let Some(fr) = decode_frame(&f, id) else { continue };
                    let (fw, fh) = (fr.width(), fr.height());
                    let sc = (120.0 / fw as f32).min(160.0 / fh as f32).min(1.0);
                    let (dw, dh) = ((fw as f32 * sc) as u32, (fh as f32 * sc) as u32);
                    let small = image::imageops::resize(&fr, dw.max(1), dh.max(1), image::imageops::FilterType::Nearest);
                    let ox = cx + (k as u32 % 2) * 70 + 1;
                    let oy = cy + 16 + (k as u32 / 2) * 92;
                    for y in 0..dh.min(90) {
                        for x in 0..dw.min(68) {
                            let p = small.get_pixel(x, y).0;
                            if p[3] > 40 {
                                img.put_pixel(ox + x, oy + y, image::Rgba(p));
                            }
                        }
                    }
                }
            }
            if let Some(s) = sheet.take() {
                s.save(format!("/tmp/sheets/{ch}_last.png")).unwrap();
            }
            println!("{ch}: {} scenes", files.len());
        }
    }

    /// Skin-ratio per scene: max over 4 sampled frames of fraction of opaque
    /// pixels in Tahni-skin RGB range. Ranks scenes for visual audit.
    #[test]
    #[ignore]
    fn skin_rank() {
        let root = format!("{}/assets/characters", env!("CARGO_MANIFEST_DIR"));
        for ch in ["tahni", "maeka", "kahli", "fatb"] {
            let dir = format!("{root}/{ch}/scenes");
            let mut files: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
            files.sort();
            let mut rows: Vec<(f64, String)> = Vec::new();
            for path in &files {
                let name = path.file_name().unwrap().to_string_lossy().replace(".bin.gz", "");
                let gz = std::fs::File::open(path).unwrap();
                let mut raw = Vec::new();
                std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(gz), &mut raw).unwrap();
                let Some(f) = parse_faz(&raw) else { continue };
                let mut ids: Vec<u16> = f.frames.keys().copied().collect();
                ids.sort();
                if ids.is_empty() { continue; }
                let mut best = 0.0f64;
                for frac in [0.3f32, 0.55, 0.8, 0.95] {
                    let id = ids[((ids.len() as f32 * frac) as usize).min(ids.len() - 1)];
                    let Some(fr) = decode_frame(&f, id) else { continue };
                    let (mut skin, mut op) = (0u64, 0u64);
                    for px in fr.pixels() {
                        let [r, g, b, a] = px.0;
                        if a > 40 {
                            op += 1;
                            if (r > 190 && g > 120 && g < 210 && b > 90 && b < 190 && r > g && g > b) || (r >= 230 && g >= 170 && g <= 235 && b >= 130 && b <= 200) {
                                skin += 1;
                            }
                        }
                    }
                    if op > 0 {
                        best = best.max(skin as f64 / op as f64);
                    }
                }
                rows.push((best, name));
            }
            rows.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
            println!("### {ch}");
            for (r, n) in rows {
                println!("{r:.3} {n}");
            }
        }
    }
}

#[cfg(test)]
mod skin_probe {
    use super::*;
    #[test]
    #[ignore]
    fn tahni_colors() {
        let path = format!("{}/assets/characters/tahni/scenes/beach.bin.gz", env!("CARGO_MANIFEST_DIR"));
        let gz = std::fs::File::open(&path).unwrap();
        let mut raw = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(gz), &mut raw).unwrap();
        let f = parse_faz(&raw).unwrap();
        let mut ids: Vec<u16> = f.frames.keys().copied().collect();
        ids.sort();
        let fr = decode_frame(&f, ids[ids.len() / 2]).unwrap();
        let mut hist = std::collections::HashMap::new();
        for px in fr.pixels() {
            if px.0[3] > 40 {
                *hist.entry(px.0).or_insert(0u64) += 1;
            }
        }
        let mut v: Vec<_> = hist.into_iter().collect();
        v.sort_by_key(|(_, n)| -(*n as i64));
        for (c, n) in v.iter().take(12) {
            println!("{c:?} {n}");
        }
    }
}

/// Large audit strips for ranked scenes: 4 frames per scene at ~2x sheet size.
#[cfg(test)]
mod audit_tests {
    use super::*;
    use ab_glyph::{Font as _, FontArc, PxScale, ScaleFont};
    use image::ImageBuffer;

    fn put_text(img: &mut RgbaImage, font: &FontArc, text: &str, x: u32, y: u32, sz: f32) {
        let scale = PxScale { x: sz, y: sz };
        let sf = font.as_scaled(scale);
        let mut pen = x as f32;
        for g in text.chars() {
            let gid = sf.glyph_id(g);
            let glyph = gid.with_scale_and_position(scale, ab_glyph::point(pen, y as f32));
            if let Some(v) = font.outline_glyph(glyph) {
                let b = v.px_bounds();
                v.draw(|gx, gy, cov| {
                    let px = b.min.x as i32 + gx as i32;
                    let py = b.min.y as i32 + gy as i32;
                    if cov > 0.4 && px >= 0 && py >= 0 && (px as u32) < img.width() && (py as u32) < img.height() {
                        img.put_pixel(px as u32, py as u32, image::Rgba([255, 255, 0, 255]));
                    }
                });
            }
            pen += sf.h_advance(gid);
        }
    }

    fn is_placeholder_faz(ch: &str, name: &str) -> bool {
        let path = format!("{}/assets/characters/{ch}/scenes/{name}.bin.gz", env!("CARGO_MANIFEST_DIR"));
        let Ok(gz) = std::fs::File::open(&path) else { return true };
        let mut raw = Vec::new();
        if std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(gz), &mut raw).is_err() { return true; }
        match parse_faz(&raw) { Some(f) => crate::faz::is_placeholder(&f), None => true }
    }

    fn strip(ch: &str, name: &str, font: &FontArc) -> Option<RgbaImage> {
        let path = format!("{}/assets/characters/{ch}/scenes/{name}.bin.gz", env!("CARGO_MANIFEST_DIR"));
        let gz = std::fs::File::open(&path).ok()?;
        let mut raw = Vec::new();
        std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(gz), &mut raw).ok()?;
        let f = parse_faz(&raw)?;
        let mut ids: Vec<u16> = f.frames.keys().copied().collect();
        ids.sort();
        if ids.is_empty() {
            return None;
        }
        let mut out = ImageBuffer::from_pixel(4 * 170, 230, image::Rgba([30, 30, 30, 255]));
        put_text(&mut out, font, name, 2, 2, 20.0);
        for (k, frac) in [0.3f32, 0.55, 0.8, 0.97].iter().enumerate() {
            let id = ids[((ids.len() as f32 * *frac) as usize).min(ids.len() - 1)];
            let Some(fr) = decode_frame(&f, id) else { continue };
            let (fw, fh) = (fr.width(), fr.height());
            let sc = (165.0 / fw as f32).min(200.0 / fh as f32).min(1.0);
            let (dw, dh) = ((fw as f32 * sc) as u32, (fh as f32 * sc) as u32);
            let small = image::imageops::resize(&fr, dw.max(1), dh.max(1), image::imageops::FilterType::Nearest);
            for y in 0..dh {
                for x in 0..dw {
                    let p = small.get_pixel(x, y).0;
                    if p[3] > 40 {
                        out.put_pixel(k as u32 * 170 + 2 + x, 24 + y, image::Rgba(p));
                    }
                }
            }
        }
        Some(out)
    }

    #[test]
    #[ignore]
    fn audit_tahni() {
        let ch = std::env::var("AUDIT_CHAR").unwrap_or_else(|_| "tahni".into());
        let font = FontArc::try_from_vec(
            std::fs::read(format!("{}/assets/fonts/LiberationSans-Regular.ttf", env!("CARGO_MANIFEST_DIR"))).unwrap(),
        )
        .unwrap();
        let mut scenes: Vec<String> = std::fs::read_dir(format!("{}/assets/characters/{ch}/scenes", env!("CARGO_MANIFEST_DIR")))
            .unwrap()
            .map(|e| e.unwrap().path().file_name().unwrap().to_string_lossy().replace(".bin.gz", ""))
            .filter(|n| !n.starts_with('.'))
            .collect();
        scenes.sort();
        if let Ok(only) = std::env::var("AUDIT_ONLY") {
            let want: Vec<&str> = only.split(',').collect();
            scenes.retain(|s| want.iter().any(|w| s == w));
        }
        let mut rows: Vec<(String, RgbaImage)> = Vec::new();
        for s in &scenes {
            let Some(st) = strip(&ch, s, &font) else { continue };
            if is_placeholder_faz(&ch, s) { continue; }
            rows.push((s.clone(), st));
        }
        let per = 6;
        for (idx, chunk) in rows.chunks(per).enumerate() {
            let mut sheet = ImageBuffer::from_pixel(680, (per * 230) as u32, image::Rgba([10, 10, 10, 255]));
            for (i, (name, st)) in chunk.iter().enumerate() {
                for y in 0..230 {
                    for x in 0..680 {
                        sheet.put_pixel(x, i as u32 * 230 + y, *st.get_pixel(x, y));
                    }
                }
            }
            sheet.save(format!("/tmp/audit/{ch}_{idx:02}.png")).unwrap();
        }
        println!("{} strips", rows.len());
    }
}

#[cfg(test)]
mod fas_probe {
    use super::*;
    #[test]
    #[ignore]
    fn parse_common_fas() {
        let raw = std::fs::read("/tmp/tahni_fas_common.FAS").unwrap();
        let f = parse_faz(&raw).expect("parse FAS");
        println!("name={} frames={} dibs={} pairs={}", f.name, f.frames.len(), f.dibs.len(), f.pairs.len());
        let (entry, defs) = script_defs(&f);
        println!("entry={:?}", entry);
        println!("defs={}", defs.len());
        for (k, v) in defs.iter().take(12) {
            println!("  {k} => {}", String::from_utf8_lossy(&v[..v.len().min(90)]));
        }
    }
}

/// Resolve `{a%2|b|c*3}` weighted-choice groups by rolling one branch
/// (weight = `%N`, default 1). Shipped scripts contain no nested groups;
/// a group with braces inside is left untouched.
pub fn resolve_choices(s: &[u8], rng: &mut impl rand::Rng) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0usize;
    while i < s.len() {
        if s[i] == b'{' {
            if let Some(end) = s[i + 1..].iter().position(|&c| c == b'}') {
                let group = &s[i + 1..i + 1 + end];
                if !group.contains(&b'{') {
                    let mut parts: Vec<&[u8]> = Vec::new();
                    let mut start = 0usize;
                    for (j, &c) in group.iter().enumerate() {
                        if c == b'|' {
                            parts.push(&group[start..j]);
                            start = j + 1;
                        }
                    }
                    parts.push(&group[start..]);
                    // (weight, length-without-weight-suffix) per part
                    let meta: Vec<(u32, usize)> = parts
                        .iter()
                        .map(|p| match p.iter().rposition(|&c| c == b'%') {
                            Some(k) if k + 1 < p.len() && p[k + 1..].iter().all(|c| c.is_ascii_digit()) => {
                                let w: u32 = std::str::from_utf8(&p[k + 1..])
                                    .ok()
                                    .and_then(|x| x.parse().ok())
                                    .unwrap_or(1);
                                (w.max(1), k)
                            }
                            _ => (1, p.len()),
                        })
                        .collect();
                    let total: u32 = meta.iter().map(|(w, _)| *w).sum();
                    let mut pick = rng.random_range(0..total);
                    let mut idx = meta.len() - 1;
                    for (k, (w, _)) in meta.iter().enumerate() {
                        if pick < *w {
                            idx = k;
                            break;
                        }
                        pick -= *w;
                    }
                    out.extend_from_slice(&parts[idx][..meta[idx].1]);
                    i = i + 1 + end + 1;
                    continue;
                }
            }
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod choice_tests {
    use super::resolve_choices;
    #[test]
    fn weighted_choice_and_passthrough() {
        let mut r = rand::rng();
        // no braces: unchanged
        assert_eq!(resolve_choices(b"0000,blink*2", &mut r), b"0000,blink*2");
        // always the only branch, weight stripped
        assert_eq!(resolve_choices(b"{solo%4}", &mut r), b"solo");
        // deterministic single pick from two, weight suffix removed
        let got = resolve_choices(b"x,{a|b%9999},y", &mut r);
        assert!(got == b"x,a,y".to_vec() || got == b"x,b,y".to_vec(), "{got:?}");
        // single-branch group: braces and weight stripped
        assert_eq!(resolve_choices(b"{keep}", &mut r), b"keep");
    }
}


#[cfg(test)]
mod audit_strips {
    use super::*;
    /// Full-size 6-frame strips for borderline scenes flagged from the
    /// contact sheets, for final gating decisions.
    #[test]
    #[ignore]
    fn borderline_strips() {
        let scenes = [
            ("kahli", "kahli_s_predator"), ("kahli", "kahli_s_jetsuit"), ("kahli", "kahli_s_spin"),
            ("kahli", "kahli_s_supergirl"), ("kahli", "kahli_s_todo"), ("kahli", "kahli_s_wireframe"),
        ];
        for (ch, sc) in scenes {
            let path = format!("{}/assets/characters/{ch}/scenes/{sc}.bin.gz", env!("CARGO_MANIFEST_DIR"));
            let Ok(gz) = std::fs::File::open(&path) else { println!("{sc}: MISSING"); continue };
            let mut raw = Vec::new();
            std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(gz), &mut raw).unwrap();
            let Some(f) = parse_faz(&raw) else { continue };
            let mut ids: Vec<u16> = f.frames.keys().copied().collect();
            ids.sort();
            let mut strip = image::ImageBuffer::<image::Rgba<u8>, Vec<u8>>::from_pixel(6 * 200, 260, image::Rgba([30, 30, 30, 255]));
            for (k, frac) in [0.1f32, 0.25, 0.4, 0.55, 0.7, 0.9].iter().enumerate() {
                let id = ids[((ids.len() as f32 * *frac) as usize).min(ids.len() - 1)];
                let Some(fr) = decode_frame(&f, id) else { continue };
                let (w, h) = (fr.width(), fr.height());
                for y in 0..h.min(260) {
                    for x in 0..w.min(200) {
                        let p = fr.get_pixel(x, y).0;
                        if p[3] > 40 {
                            strip.put_pixel(x + k as u32 * 200, y, image::Rgba(p));
                        }
                    }
                }
            }
            strip.save(format!("/tmp/strips/{sc}.png")).unwrap();
            println!("{sc}: {} frames", ids.len());
        }
    }
}

#[cfg(test)]
mod touch_strips {
    use super::*;
    /// Ignored diagnostic: 12-frame strip for every *named* touch
    /// hotspot def (voiced or silent; frames live in the touch.FAS
    /// container itself), for visual gating decisions on the
    /// suggestive zones.
    #[test]
    #[ignore]
    fn touch_strips() {
        for ch in ["tahni", "maeka", "kahli", "fatb"] {
            // reaction frames can live in touch.FAS or touch_demo.FAS
            let mut gs: Vec<Faz> = Vec::new();
            for cf in ["touch.fas.gz", "touch_demo.fas.gz"] {
                let Ok(gz) = std::fs::File::open(format!(
                    "{}/assets/characters/{ch}/behaviors/{cf}",
                    env!("CARGO_MANIFEST_DIR")
                )) else { continue };
                let mut raw = Vec::new();
                std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(gz), &mut raw).unwrap();
                if let Some(g) = parse_faz(&raw) {
                    gs.push(g);
                }
            }
            let mut all: Vec<(String, Vec<u8>)> = Vec::new();
            for g in &gs {
                let (_e, defs) = script_defs(g);
                for (k, b) in defs {
                    all.push((k.clone(), b.clone()));
                }
            }
            let mut seen: std::collections::HashSet<String> = Default::default();
            for (name, body) in all.iter() {
                // named hotspot defs only: T0x......_Name (voiced or
                // silent — the silent DEMO variants need gating too)
                if !name.starts_with("T0x") || !name.contains('_') {
                    continue;
                }
                if !seen.insert(name.clone()) {
                    continue;
                }
                // frame ids referenced by the body (4-5 digit runs)
                let mut ids: std::collections::BTreeSet<u32> = Default::default();
                let bs: &[u8] = body;
                let mut p = 0usize;
                while p < bs.len() {
                    if bs[p].is_ascii_digit() {
                        let (v, n) = read_digits(bs, p, 5);
                        if n >= 4 {
                            ids.insert(v);
                            p += n;
                            continue;
                        }
                    }
                    p += 1;
                }
                let ids: Vec<u16> = ids
                    .into_iter()
                    .filter(|i| gs.iter().any(|g| g.frames.contains_key(&(*i as u16))))
                    .map(|i| i as u16)
                    .collect();
                if ids.is_empty() {
                    println!("{ch}/{name}: no frames");
                    continue;
                }
                let mut strip = image::ImageBuffer::<image::Rgba<u8>, Vec<u8>>::from_pixel(12 * 200, 260, image::Rgba([30, 30, 30, 255]));
                for (k, frac) in [0.04f32, 0.12, 0.2, 0.28, 0.36, 0.44, 0.52, 0.6, 0.68, 0.76, 0.88, 0.98].iter().enumerate() {
                    let id = ids[((ids.len() as f32 * *frac) as usize).min(ids.len() - 1)];
                    let Some(fr) = gs.iter().find_map(|g| decode_frame(g, id)) else { continue };
                    let (w, h) = (fr.width(), fr.height());
                    for y in 0..h.min(260) {
                        for x in 0..w.min(200) {
                            let p = fr.get_pixel(x, y).0;
                            if p[3] > 40 {
                                strip.put_pixel(x + k as u32 * 200, y, image::Rgba(p));
                            }
                        }
                    }
                }
                strip.save(format!("/tmp/tstrips/{ch}_{name}.png")).unwrap();
            }
        }
    }
}

#[cfg(test)]
mod touch_tests {
    use super::*;

    /// touch.FAS must yield its hotspot bitmap and the `T0x…` reaction defs
    /// (the click-to-sound wiring depends on both).
    #[test]
    fn touch_fas_parses() {
        let raw = {
            let f = std::fs::File::open(format!(
                "{}/assets/characters/tahni/behaviors/touch.fas.gz",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap();
            let mut b = Vec::new();
            std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(f), &mut b).unwrap();
            b
        };
        let f = parse_faz(&raw).unwrap();
        let t = f.touch.as_ref().expect("touch bitmap parsed");
        assert_eq!((t.width, t.height, t.bitmap.len()), (200, 200, 40000));
        assert_eq!(t.colors.len(), 256);
        let (_e, defs) = script_defs(&f);
        assert!(defs.len() > 100, "got {} defs", defs.len());
        let names: Vec<&String> = defs.iter().map(|(k, _)| k).filter(|k| k.starts_with("T0x")).collect();
        assert!(names.len() > 100);
        // The bitmap's color indices must resolve to colors that name defs.
        let mut hit = false;
        for (k, _) in &defs {
            if let Some(rest) = k.strip_prefix("T0x") {
                let hex: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
                let v = u32::from_str_radix(&hex, 16).unwrap();
                if t.colors.contains(&v) {
                    hit = true;
                    break;
                }
            }
        }
        assert!(hit, "no def color present in hotspot palette");
    }
}



