#!/usr/bin/env python3
"""Survey classic (early-set) scene candidates vs the shipped scene set.

For each character, reads the early-set sources (the deskmate_202601
bundle's <Char>/Data/*.FAZ plus any demo dirs), unwraps zlib-wrapped
Install-Creator streams exactly like pack_assets.py, applies the same
scene filter, and compares md5 of the FAZ bytes against the md5 of the
gunzipped shipped assets/characters/<char>/scenes/*.bin.gz.

Prints per character: byte-identical (dedup) count, new scenes, and
name collisions whose bytes differ (those must NOT overwrite shipped
scenes).

Usage: python3 analysis/classic_survey.py [character ...]
"""
import gzip
import hashlib
import os
import re
import sys
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import faz

BUNDLE = os.path.expanduser(
    '~/reference/oska-deskmates/ia-finds/deskmate_202601/extracted/'
    'Deskmate/unpack-all/Deskmate - All Full Deskmates Cracked')
IAFINDS = os.path.expanduser('~/reference/oska-deskmates/ia-finds')
CD = os.path.join(IAFINDS, 'computer200008cd/extracted/Computer/hry/tahni-v104-cd')

# character -> list of early-set source dirs (each scanned for *.FAZ/*.fas)
SOURCES = {
    'tahni': [os.path.join(BUNDLE, 'Tahni', 'Data'),
              os.path.join(IAFINDS, 'tahni-104'),
              CD],
    'teecee': [os.path.join(BUNDLE, 'TeeCee', 'Data')],
    'fatb': [os.path.join(BUNDLE, 'FatB', 'Data')],
    'kahli': [os.path.join(BUNDLE, 'Kahli', 'Data')],
    'maeka': [os.path.join(BUNDLE, 'Maeka', 'Data')],
    'oska': [os.path.join(BUNDLE, 'Oska', 'Data')],
}

ZLIB_MAGICS = (b'\x78\x9c', b'\x78\xda', b'\x78\x01')
NAME_RE = re.compile(r'^[A-Za-z][\w]*$')


def unwrap(raw):
    """FAZ bytes for a source file: unwrapped stream, else raw."""
    if len(raw) > 6 and raw[4:6] in ZLIB_MAGICS:
        try:
            return zlib.decompress(raw[4:])
        except zlib.error:
            return None
    return raw


def shipped_md5s(char):
    out = {}
    d = os.path.join(ROOT, 'assets', 'characters', char, 'scenes')
    for f in sorted(os.listdir(d)):
        if not f.endswith('.bin.gz'):
            continue
        raw = gzip.open(os.path.join(d, f), 'rb').read()
        out[hashlib.md5(raw).hexdigest()] = f[:-len('.bin.gz')]
    return out


def survey(char):
    ship = shipped_md5s(char)
    ship_names = {n for n in ship.values()}
    ident, new, collide = {}, {}, {}
    for src in SOURCES[char]:
        if not os.path.isdir(src):
            print(f'  !! missing source dir {src}')
            continue
        for f in sorted(os.listdir(src)):
            if f.split('.')[-1].upper() not in ('FAZ', 'FAS'):
                continue
            raw = unwrap(open(os.path.join(src, f), 'rb').read())
            if raw is None:
                continue
            try:
                r = faz.parse_faz(raw)
            except Exception:
                continue
            if not r['frames'] and not r['pairs']:
                continue
            if r['palette'] is None:
                continue
            sname = r['name']
            if not sname.isascii() or not NAME_RE.match(sname):
                continue
            if sname.startswith('Tahni3D'):
                continue  # 3D float scenes belong to deskmates3d-rs
            if sname in ('common', 'common_demo', 'common_enter',
                         'common_enter_demo', 'touch', 'touch_demo'):
                continue  # behavior scripts, already shipped as behaviors/
            md5 = hashlib.md5(raw).hexdigest()
            entry = (sname, md5, len(raw), f, src)
            if md5 in ship:
                ident[sname] = entry
            elif sname in ship_names:
                collide[sname] = entry
            else:
                new[sname] = entry
    print(f'== {char}: shipped={len(ship)} identical={len(ident)} '
          f'collide-differ={len(collide)} new={len(new)}')
    for s in sorted(collide):
        print(f'  COLLIDE {s}: {collide[s][3]} ({collide[s][2]} B) from {collide[s][4]}')
    for s in sorted(new):
        print(f'  NEW {s}: {new[s][3]} ({new[s][2]} B) from {new[s][4]}')
    return ship, ident, collide, new


if __name__ == '__main__':
    chars = sys.argv[1:] or list(SOURCES)
    for c in chars:
        survey(c)
