#!/usr/bin/env python3
"""Pack the classic (early-set) scenes into the shipped asset layout.

Classic mode wires the early-version DeskMate scene sets in as extra
scenes alongside the current ones (see analysis/CLASSIC_MODE.md for
provenance and the label table). Each entry below is a scene that is NOT
byte-identical to anything already shipped (analysis/classic_survey.py
dedup); scenes whose FAZ-internal name collides with a shipped scene are
stored under a `classic_` prefix — scenes.rs keys scenes by filename
stem, so both versions coexist.

Writes gzip of the original (stream-unwrapped) FAZ bytes to
`assets/characters/<char>/scenes/<target>.bin.gz`, exactly the packing
convention of pack_assets.py.

Usage: python3 analysis/pack_classic.py [character ...]   (default: all)
"""
import gzip
import os
import sys
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import faz

IAFINDS = os.path.expanduser('~/reference/oska-deskmates/ia-finds')
BUNDLE = os.path.join(IAFINDS, 'deskmate_202601/extracted/Deskmate/'
                       'unpack-all/Deskmate - All Full Deskmates Cracked')
CD = os.path.join(IAFINDS, 'computer200008cd/extracted/Computer/hry/tahni-v104-cd')

# character -> [(source file, target scene name)]
CLASSIC = {
    'tahni': [
        # Czech Computer 2000/08 cover CD: the original v1.04 demo
        # distribution (Apr 2000) — the earliest Tahni artifacts known.
        (os.path.join(CD, 'bull.FAZ'), 'classic_bull'),
        (os.path.join(CD, 'rollerblade.FAZ'), 'classic_rollerblade'),
        (os.path.join(CD, 'sailing.FAZ'), 'classic_sailing'),
        (os.path.join(CD, 'scuba.FAZ'), 'classic_scuba'),
        (os.path.join(CD, 'snowboard.FAZ'), 'classic_snowboard'),
        (os.path.join(CD, 'space_hopper.FAZ'), 'classic_space_hopper'),
        # tahni-104 (v1.04 full unlock data): scenes dropped from later
        # sets, plus the v1.04 variants of buggy/dustbuster (the v1.04
        # scuba is superseded by the CD cut; the v1.04 arnie is a 1-frame
        # placeholder and is skipped).
        (os.path.join(IAFINDS, 'tahni-104/Tahni Data__bambi_flash.FAZ'), 'bambi_flash'),
        (os.path.join(IAFINDS, 'tahni-104/Tahni Data__bambii_champ.FAZ'), 'bambii_champ'),
        (os.path.join(IAFINDS, 'tahni-104/Tahni Data__bambii_promo.FAZ'), 'bambii_promo'),
        (os.path.join(IAFINDS, 'tahni-104/Tahni Data__buggy.FAZ'), 'classic_buggy'),
        (os.path.join(IAFINDS, 'tahni-104/Tahni Data__dustbuster.FAZ'), 'classic_dustbuster'),
    ],
}

ZLIB_MAGICS = (b'\x78\x9c', b'\x78\xda', b'\x78\x01')


def unwrap(raw):
    if len(raw) > 6 and raw[4:6] in ZLIB_MAGICS:
        return zlib.decompress(raw[4:])
    return raw


def pack(char):
    out = os.path.join(ROOT, 'assets', 'characters', char, 'scenes')
    total = 0
    for src, target in CLASSIC[char]:
        d = unwrap(open(src, 'rb').read())
        r = faz.parse_faz(d)
        assert r['name'], f'{src}: no scene name'
        assert r['frames'] and r['palette'], f'{src}: not a playable scene'
        assert len(r['frames']) > 1, f'{src}: placeholder (<=1 frame)'
        blob = gzip.compress(d, 9, mtime=0)
        total += len(blob)
        with open(os.path.join(out, target + '.bin.gz'), 'wb') as fh:
            fh.write(blob)
        print(f'  {char}/{target:24s} <- {os.path.basename(src)} '
              f'({len(blob) / 1e6:.2f} MB gz)')
    print(f'{char}: {len(CLASSIC[char])} classic scenes -> {out} '
          f'({total / 1e6:.1f} MB)')


if __name__ == '__main__':
    want = sys.argv[1:] or list(CLASSIC)
    for c in want:
        pack(c)
