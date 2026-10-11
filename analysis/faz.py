#!/usr/bin/env python3
"""FAZ scene container parser (Oska DeskMates format).

Layout: name\0 | header | N x DIB template (40-byte header + 16-entry palette, 104B each)
        | u32 offset table | script (null-separated name/value string pairs)
        | frame table (u16 ids, terminated by 0xffff) | flags (1B/id)
        | frame pixel blocks (200*200/2 bytes each, 4bpp bottom-up, block k = ids[k]).
"""
import struct, re

MAXSTR = 4000

def _printable(b):
    return all(9 <= c <= 13 or 32 <= c <= 255 for c in b)

def parse_faz(d):
    z = d.index(b'\x00')
    name = d[:z].decode('latin1')
    # DIB templates
    i = z + 1
    while i < 2000 and d[i:i+4] != b'\x28\x00\x00\x00':
        i += 1
    dibs = []
    while i + 40 <= len(d) and d[i:i+4] == b'\x28\x00\x00\x00':
        w, h, pl, bpp = struct.unpack_from('<iiHH', d, i+4)
        if (w, h, bpp) != (200, 200, 4):
            break
        pal = struct.unpack_from('<16I', d, i+40)
        dibs.append((i, pal))
        i += 104
    # script: first position where >=3 consecutive null-term printable strings occur
    s = i
    found = False
    while s < len(d) - 4 and not found:
        p = s
        ok = True
        for _ in range(3):
            e = d.find(b'\x00', p)
            if e == -1 or e - p < 1 or e - p > MAXSTR or not _printable(d[p:e]):
                ok = False
                break
            p = e + 1
        if ok:
            found = True
            break
        s += 1
    pairs = []
    while s < len(d):
        e = d.find(b'\x00', s)
        if e == -1 or e - s > MAXSTR:
            break
        t = d[s:e]
        if not t or not _printable(t):
            break
        pairs.append(t.decode('latin1'))
        s = e + 1
    # frame table
    ids = []
    i = s
    while i + 2 <= len(d):
        v = struct.unpack_from('<H', d, i)[0]
        if v == 0xffff:
            break
        ids.append(v)
        i += 2
    flags = d[i:i+len(ids)]
    data = d[i+len(ids):]
    bs = 20000
    nblocks = len(data) // bs
    frames = {}
    for k in range(min(nblocks, len(ids))):
        frames[ids[k]] = data[k*bs:(k+1)*bs]
    pal = dibs[0][1] if dibs else None
    return dict(name=name, dibs=dibs, pairs=pairs, ids=ids, frames=frames, palette=pal)

if __name__ == '__main__':
    import sys
    d = open(sys.argv[1], 'rb').read()
    f = parse_faz(d)
    print('name', f['name'], 'dibs', len(f['dibs']), 'ids', len(f['ids']),
          'frames', len(f['frames']), 'pairs', len(f['pairs']))
    for p in f['pairs'][:20]:
        print(' ', p[:100])
