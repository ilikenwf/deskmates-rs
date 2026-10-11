#!/usr/bin/env python3
"""Unpack the newer Oska DeskMates installer payload streams (ClickTeam 'wwgT'
Install Creator variant — Maeka, Kahli, Tavia, Arianna, Dancers, Sirens, ...).

Input: a directory of payload streams produced by ic4_extract.py (NNNN.bin).
Stream kinds found there:
  * scene streams: [u32 uncompressed size][zlib stream] -> plain FAZ bytes
    (the FAZ frame table uses the new count-prefixed form; see faz.py).
  * FAS masters (common, common_demo, common_enter*, touch, touch_demo):
    plain FAZ bytes, not scene files — skipped here (the engine takes entry
    points from each scene's own `all` defs).
  * misc resources (config blob, MZ runtimes, 'SPACER'/'M001' index
    containers): skipped.

Scenes that carry no pixel blocks (palette-swap variants whose frames live in
the still unpacked M001 container) are skipped; all frame geometries the
engine supports (any sane w/h at 4 or 8 bpp) are converted.

Usage: python3 maeka.py <stream_dir> <out_dir>
Then: python3 faz_convert.py <assets/characters/NAME> <out_dir>
"""
import os, sys, zlib, struct

ZLIB_MAGICS = (b'\x78\x9c', b'\x78\xda', b'\x78\x01')

def unwrap(raw):
    """Return the FAZ bytes inside a scene stream, or None if not one."""
    if len(raw) > 6 and raw[4:6] in ZLIB_MAGICS:
        try:
            return zlib.decompress(raw[4:])
        except zlib.error:
            return None
    return None

def faz_name(d):
    z = d.find(b'\x00')
    if z <= 0 or z > 64:
        return None
    try:
        name = d[:z].decode('ascii')
    except UnicodeDecodeError:
        return None
    if not name.replace('_', '').isalnum():
        return None
    return name

def main():
    src, out = sys.argv[1], sys.argv[2]
    os.makedirs(out, exist_ok=True)
    n = 0
    for f in sorted(os.listdir(src)):
        if not f.endswith('.bin'):
            continue
        d = unwrap(open(os.path.join(src, f), 'rb').read())
        if d is None:
            continue
        name = faz_name(d)
        if not name:
            continue
        i = d.find(b'\x00') + 1
        while i < 3000 and d[i:i+4] != b'\x28\x00\x00\x00':
            i += 1
        if d[i:i+4] != b'\x28\x00\x00\x00':
            print(f, name, 'skip: no DIB template')
            continue
        w, h, pl, bpp = struct.unpack_from('<iiHH', d, i+4)
        if bpp not in (4, 8) or not (0 < w <= 1024 and 0 < h <= 1024):
            print(f, name, f'skip: geometry {w}x{h}x{bpp}')
            continue
        # frame count via the count-prefixed table: scan plausible header spots
        bs = ((w * bpp // 8) + 3) & ~3
        bs *= h
        cnt = None
        for c in range(i, len(d) - 4):
            v = struct.unpack_from('<I', d, c)[0]
            if v > 4000:
                continue
            if len(d) - (c + 4) == v * (bs + 3):
                cnt = v
                break
        if not cnt:
            print(f, name, 'skip: no frame blocks (variant scene)')
            continue
        open(os.path.join(out, name + '.bin'), 'wb').write(d)
        print(f, '->', name, cnt, 'frames')
        n += 1
    print(f'{src}: {n} scene FAZs -> {out}/')

if __name__ == '__main__':
    main()
