#!/usr/bin/env python3
"""FAZ scene container parser (Oska DeskMates format).

Layout: name\0 | header | N x DIB template (40-byte header + 16-entry palette, 104B each)
        | script (null-separated name/value string pairs)
        | frame table | frame pixel blocks (4bpp bottom-up, stride-padded rows,
          block k = ids[k]).
Frame table, old format (Tahni/FatB installers): u16 ids terminated by 0xffff,
then 1 flag byte per id. New format (wwgT installers, see maeka.py): u32 count,
count u16 ids, count flag bytes, no terminator. The count form is detected by
exact size validation, falling back to the 0xffff scan.
"""
import struct, re

MAXSTR = 4000

def _printable(b):
    return all(9 <= c <= 13 or 32 <= c <= 255 for c in b)

def block_size(w, h, bpp):
    """Bytes per pixel block: rows padded to a 4-byte stride."""
    row = ((w * bpp // 8) + 3) & ~3
    return row * h

def parse_faz(d):
    z = d.index(b'\x00')
    name = d[:z].decode('latin1')
    # DIB templates
    i = z + 1
    while i < 2000 and d[i:i+4] != b'\x28\x00\x00\x00':
        i += 1
    dibs = []
    dims = None
    bpp_ = None
    while i + 40 <= len(d) and d[i:i+4] == b'\x28\x00\x00\x00':
        w, h, pl, bpp = struct.unpack_from('<iiHH', d, i+4)
        if bpp not in (4, 8) or not (0 < w <= 1024 and 0 < h <= 1024):
            break
        ncol = 16 if bpp == 4 else 256
        pal = struct.unpack_from(f'<{ncol}I', d, i+40)
        dibs.append((i, pal))
        dims = (w, h)
        bpp_ = bpp
        i += 40 + ncol * 4
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
    # frame table: try the OLD 0xffff-terminated form FIRST. Its own
    # invariants (u16 count at s-2 matching the id run, exact trailing
    # length after the flag bytes) are strict; the count-form size
    # equation false-positives on old files (u32 at s-2 = count byte +
    # three NULs of the leading id 0 satisfies len-(c+4)==v*(bs+3)),
    # dropping the leading 0 id and shifting every frame one pose.
    ids = []
    data = None
    table = 'old'
    flags = None
    bs = block_size(dims[0], dims[1], bpp_ or 4) if dims else 20000
    if s is not None and s >= 2:
        i = s
        while i + 2 <= len(d):
            v = struct.unpack_from('<H', d, i)[0]
            if v == 0xffff:
                break
            ids.append(v)
            i += 2
        else:
            i = None
        if i is not None:
            n = len(ids)
            after = i + 2
            if struct.unpack_from('<H', d, s - 2)[0] == n and \
               len(d) == after + n + n * bs:
                table = 'old'
                flags = list(d[after:after + n])
                data = d[after + n:]
            else:
                ids = []
    if data is None:
        if s is not None:
            for c in range(max(s - 8, 0), min(s + 16, len(d) - 4)):
                v = struct.unpack_from('<I', d, c)[0]
                if v > 4000:
                    continue
                if len(d) - (c + 4) == v * (bs + 3):
                    table = 'count'
                    ids = list(struct.unpack_from('<%dH' % v, d, c + 4)) if v else []
                    flags = list(d[c + 4 + 2 * v:c + 4 + 3 * v])
                    data = d[c + 4 + 3 * v:]
                    break
        if data is None:
            i = s if s is not None else len(d)
            while i + 2 <= len(d):
                v = struct.unpack_from('<H', d, i)[0]
                if v == 0xffff:
                    break
                ids.append(v)
                i += 2
            data = d[i + len(ids):]
    nblocks = len(data) // bs
    frames = {}
    frame_flags = {}
    for k in range(min(nblocks, len(ids))):
        frames[ids[k]] = data[k*bs:(k+1)*bs]
        if flags is not None:
            frame_flags[ids[k]] = flags[k]
    pal = dibs[0][1] if dibs else None
    return dict(name=name, dibs=dibs, pairs=pairs, ids=ids, frames=frames,
                palette=pal, dims=dims, bpp=bpp_, table=table,
                frame_flags=frame_flags)

if __name__ == '__main__':
    import sys
    d = open(sys.argv[1], 'rb').read()
    f = parse_faz(d)
    print('name', f['name'], 'dibs', len(f['dibs']), 'ids', len(f['ids']),
          'frames', len(f['frames']), 'pairs', len(f['pairs']))
    for p in f['pairs'][:20]:
        print(' ', p[:100])
