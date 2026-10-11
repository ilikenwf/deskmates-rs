#!/usr/bin/env python3
"""Convert Tahni FAZ scenes to PNG frames + scene JSON for the Rust engine.

Script grammar (from FAZ script pairs):
  name (ref,ref,...)          -> sequence definition
  name ([x,y]id-id*3,ref*N)  -> frame list; [x,y] offset applies to following id/range
  ref = list name | sequence name | builtin | 0000 (blank) | NNNN (4-digit list name)
  *N  -> repeat N times; N>=1000 treated as duration-ms by the engine
Builtins: exit_wait home_wait enter_home pause all all_slow all_fast
          fence_*_out (screen-edge exits), 0003 (missing common list -> blank pause)
"""
import os, re, sys, json, zlib, struct
import faz

OUT = sys.argv[1] if len(sys.argv) > 1 else 'tahni_out'
SRC = sys.argv[2] if len(sys.argv) > 2 else 'tahni_real'

BUILTINS = {'exit_wait', 'home_wait', 'enter_home', 'pause', 'all', 'all_slow',
            'all_fast', 'all_demo', 'all_slow_demo', 'all_fast_demo'}
FENCE = re.compile(r'^fence_\w+$')

def png_indexed(w, h, idx_rows, pal):
    def chunk(t, v):
        c = struct.pack('>I', len(v)) + t + v
        return c + struct.pack('>I', zlib.crc32(t + v) & 0xffffffff)
    png = b'\x89PNG\r\n\x1a\n'
    png += chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, 8, 3, 0, 0, 0))
    plte = b''.join(struct.pack('BBB', (p >> 16) & 255, (p >> 8) & 255, p & 255) for p in pal)
    png += chunk(b'PLTE', plte)
    # FAZ transparent key is palette index 8 (black); index 0 black = visible outline
    trns = bytes([255] * 8 + [0] + [255] * 7)
    png += chunk(b'tRNS', trns)
    raw = b''.join(b'\x00' + bytes(r) for r in idx_rows)
    png += chunk(b'IDAT', zlib.compress(raw, 6))
    png += chunk(b'IEND', b'')
    return png

def frame_rows(blk):
    # 4bpp bottom-up -> top-down rows of palette indices
    rows = []
    for y in range(199, -1, -1):
        r = blk[y*100:(y+1)*100]
        px = bytearray()
        for b in r:
            px.append(b >> 4); px.append(b & 15)
        rows.append(bytes(px))
    return rows

REF = re.compile(r'\[(-?\d+),(-?\d+)\]|(\d{4,5})(?:-(\d{4,5}))?(?:\*(\d+))?|([a-zA-Z_][a-zA-Z0-9_]*)\*(\d+)|([a-zA-Z_][a-zA-Z0-9_]*)|\*|(\d)')

def parse_reflist(s):
    """Parse a parenthesized frame-list / sequence body into token dicts."""
    toks = []
    offx = offy = 0
    for m in REF.finditer(s):
        if m.group(1) is not None:
            offx, offy = int(m.group(1)), int(m.group(2))
            continue
        if m.group(3) is not None:
            a = int(m.group(3)); b = int(m.group(4) or a)
            rep = int(m.group(5)) if m.group(5) else 1
            step = 1 if b >= a else -1
            ids = list(range(a, b + step, step))
            for i in ids:
                toks.append(dict(id=i, x=offx, y=offy, rep=rep if i == ids[0] else 1))
            offx = offy = 0
            continue
        if m.group(6) is not None:
            toks.append(dict(name=m.group(6), rep=int(m.group(7))))
            continue
        if m.group(8) is not None:
            nm = m.group(8)
            if nm in ('pause',):
                toks.append(dict(name=nm, rep=1))
            elif nm and nm[0].isdigit():
                a = int(nm)
                toks.append(dict(id=a, x=offx, y=offy, rep=1))
                offx = offy = 0
            else:
                toks.append(dict(name=nm, rep=1))
            continue
    return toks

def main():
    os.makedirs(OUT, exist_ok=True)
    scenes_out = {}
    for f in sorted(os.listdir(SRC)):
        if not f.endswith('.bin'):
            continue
        d = open(os.path.join(SRC, f), 'rb').read()
        try:
            r = faz.parse_faz(d)
        except Exception:
            continue
        if not r['frames'] and not r['pairs']:
            continue
        if r['palette'] is None:
            continue  # no DIB template: not a scene file
        name = r['name']
        if not name.isascii() or not re.match(r'^[A-Za-z][\w]*$', name):
            continue
        if name.startswith('Tahni3D'):
            continue  # 3D float scenes: different frame geometry
        pairs = r['pairs']
        # split header pairs (before first def-looking pair) and defs
        defs = {}
        order = []
        i = 0
        while i + 1 < len(pairs):
            a, b = pairs[i], pairs[i+1]
            if b.startswith('(') or (a.isidentifier() and b and not b.startswith('{')):
                defs[a] = b
                order.append(a)
                i += 2
            else:
                i += 1
        # entry scenes: names in braces of first header pair, or top-level refs
        entry_names = []
        for a, b in [(pairs[k], pairs[k+1]) for k in range(0, len(pairs)-1, 2)]:
            m = re.match(r'^\{([\w|]+)\},pause$', b)
            if m and a in ('all', 'all_slow', 'all_fast'):
                entry_names = m.group(1).split('|')
                break
        if not entry_names:
            entry_names = [k for k in defs if k == name or k.startswith(name)]
        # write frames
        fdir = os.path.join(OUT, 'frames', name)
        os.makedirs(fdir, exist_ok=True)
        pal = r['palette']
        for fid, blk in r['frames'].items():
            open(os.path.join(fdir, f'{fid}.png'), 'wb').write(
                png_indexed(200, 200, frame_rows(blk), pal))
        # parse defs
        parsed = {}
        for k, v in defs.items():
            parsed[k] = parse_reflist(v)
        scenes_out[name] = dict(name=name, entries=entry_names, defs=parsed)
        print(name, len(entry_names), 'entries,', len(defs), 'defs,', len(r['frames']), 'frames')
    json.dump(scenes_out, open(os.path.join(OUT, 'scenes.json'), 'w'))
    print('wrote', os.path.join(OUT, 'scenes.json'))

if __name__ == '__main__':
    main()
