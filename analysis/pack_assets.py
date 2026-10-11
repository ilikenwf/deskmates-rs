#!/usr/bin/env python3
"""Pack the original FAZ scene files into the engine's asset layout.

For each character, reads its source directory (plain FAZ scene files or
Install Creator payload streams, which are unwrapped first), applies the
same scene filter faz_convert.py uses (ASCII `^[A-Za-z]\\w*$` name, has a
DIB palette, has frames or script pairs), and writes the original FAZ
bytes gzip-compressed to `assets/characters/<name>/scenes/<name>.bin.gz`.

The 3D variants (tahni3d/kahli3d) feed the separate 3D repo and are packed
into analysis/packed_<name>/ instead.

Usage: python3 analysis/pack_assets.py [character ...]   (default: all)
"""
import os, re, sys, gzip, zlib

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import faz

# character -> (source dir under analysis/, output dir override or None)
# None output -> assets/characters/<character>/scenes/
CHARS = {
    'tahni':   ('tahni_real', None),
    'fatb':    ('basic_files', None),
    'maeka':   ('maeka_faz', None),
    'kahli':   ('kahli_scenes', None),
    'tavia':   ('tavia_all', None),
    'sirens':  ('sirens_all', None),
    'fantasy': ('fantasy_all', None),
    'dm05':    ('dm05_all', None),
    'sns':     ('sns_all', None),
    'arianna': ('arianna_all', None),
    'dancers': ('streams_dancers', None),
    'tahni3d': ('streams_tahni3d', 'packed_tahni3d'),
    'kahli3d': ('streams_kahli3d', 'packed_kahli3d'),
}

ZLIB_MAGICS = (b'\x78\x9c', b'\x78\xda', b'\x78\x01')
NAME_RE = re.compile(r'^[A-Za-z][\w]*$')


def candidates(raw):
    """FAZ byte candidates for a source file: unwrapped stream, else raw."""
    if len(raw) > 6 and raw[4:6] in ZLIB_MAGICS:
        try:
            return [zlib.decompress(raw[4:])]
        except zlib.error:
            return []
    return [raw]


def pack(name, src_dir, out_override):
    src = os.path.join(HERE, src_dir)
    out = (os.path.join(HERE, out_override) if out_override
           else os.path.join(ROOT, 'assets', 'characters', name, 'scenes'))
    os.makedirs(out, exist_ok=True)
    scenes = {}
    for f in sorted(os.listdir(src)):
        if not f.endswith('.bin'):
            continue
        for d in candidates(open(os.path.join(src, f), 'rb').read()):
            try:
                r = faz.parse_faz(d)
            except Exception:
                continue
            if not r['frames'] and not r['pairs']:
                continue
            if r['palette'] is None:
                continue
            sname = r['name']
            if not sname.isascii() or not NAME_RE.match(sname):
                continue
            if not out_override and sname.startswith('Tahni3D'):
                continue  # 3D float scenes: only for the 3D repo
            scenes[sname] = d
    for old in os.listdir(out):
        if old.endswith('.bin.gz'):
            os.remove(os.path.join(out, old))
    total = 0
    for sname, d in sorted(scenes.items()):
        blob = gzip.compress(d, 9, mtime=0)
        total += len(blob)
        with open(os.path.join(out, sname + '.bin.gz'), 'wb') as fh:
            fh.write(blob)
    print(f'{name:9s} {len(scenes):3d} scenes -> {out} ({total / 1e6:.1f} MB)')


def main():
    want = sys.argv[1:] or list(CHARS)
    for name in want:
        src_dir, out_override = CHARS[name]
        pack(name, src_dir, out_override)


if __name__ == '__main__':
    main()
