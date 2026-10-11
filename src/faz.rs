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

    // script: first position with >=3 consecutive NUL-term printable strings
    let mut s = i;
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
    {
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

    Some(Faz { name, dibs, pairs, frames, dims, bpp, count_table, frame_flags })
}

// ---------------------------------------------------------------------------
// Script parsing (port of faz_convert.py main() def/entry extraction)
// ---------------------------------------------------------------------------

/// One token of a parsed def body; mirrors the dicts `parse_reflist` emits
/// (frame tokens carry id/x/y, name tokens carry name; rep defaults to 1).
pub enum Tok {
    Frame { id: u32, x: i32, y: i32, rep: u32 },
    Name { name: String, rep: u32 },
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
    // Per-frame palette for count-table scenes: dibs[2*flag] (odd DIBs are
    // tint masks); transparent key = last black (>=1) index, else 8.
    let pal: &Vec<u32>;
    let key: Option<usize>;
    match (f.count_table, f.frame_flags.get(&id)) {
        (true, Some(&flag)) => {
            pal = f.dibs.get(usize::from(flag).saturating_mul(2).min(f.dibs.len() - 1))?;
            let blacks: Vec<usize> =
                (1..pal.len()).filter(|&j| (pal[j] & 0xFFFFFF) == 0).collect();
            key = Some(blacks.last().copied().unwrap_or(8));
        }
        _ => {
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
