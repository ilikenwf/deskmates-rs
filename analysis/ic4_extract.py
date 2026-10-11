#!/usr/bin/env python3
"""Extract payload streams from newer Oska installers (Maeka.exe, Kahli_Setup.exe).

These use a ClickTeam Install Creator variant whose payload (right after the PE
sections, magic 'wwgT)H') is a sequence of concatenated zlib / bzip2 streams
rather than the V5 0x7f7f entry table. Entry headers are inconsistent, so we
just scan for stream starts and decompress each fully.

Scene streams in these installers are NOT plain FAZ (offset-table prefixed,
possibly encrypted) — format RE in progress; see MENAGERIE-ROADMAP.md.
"""
import sys, os, zlib, bz2

def extract(exe, out):
    d = open(exe, 'rb').read()
    import struct
    e = struct.unpack_from('<I', d, 0x3c)[0]
    nsec = struct.unpack_from('<H', d, e + 6)[0]
    optsz = struct.unpack_from('<H', d, e + 20)[0]
    sec = e + 24 + optsz
    end = 0
    for i in range(nsec):
        end = max(end, struct.unpack_from('<I', d, sec + 40*i + 16)[0] +
                       struct.unpack_from('<I', d, sec + 40*i + 20)[0])
    os.makedirs(out, exist_ok=True)
    i, n = end, 0
    while i < len(d) - 4:
        j, p = -1, None
        for pat in (b'\x78\x9c', b'\x78\xda', b'\x78\x01', b'BZh9'):
            k = d.find(pat, i)
            if k >= 0 and (j < 0 or k < j):
                j, p = k, pat
        if j < 0:
            break
        try:
            if p == b'BZh9':
                do = bz2.BZ2Decompressor()
                out_b = do.decompress(d[j:j + 32_000_000])
                used = len(d[j:j + 32_000_000]) - len(do.unused_data)
            else:
                do = zlib.decompressobj()
                out_b = do.decompress(d[j:j + 32_000_000])
                used = len(d[j:j + 32_000_000]) - len(do.unused_data)
        except Exception:
            i = j + 1
            continue
        open(os.path.join(out, f'{n:04d}.bin'), 'wb').write(out_b)
        n += 1
        i = j + used
    print(f'{exe}: {n} streams -> {out}/')

if __name__ == '__main__':
    extract(sys.argv[1], sys.argv[2])
