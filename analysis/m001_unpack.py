#!/usr/bin/env python3
"""Recover the Maeka palette-swap variant scenes (dark, dummy, firedance,
ignius, knife, rune, splash, strikes, veridius, white).

Background / format findings
---------------------------
The variant streams in the Maeka.exe payload (ic4_extract.py output) are
truncated FAZ files: name + 18 DIB palettes + full script pairs, then a
5-byte stub `[u32 N][u8 0]` where the frame table should be (N is not a
frame count — the files hold no blocks and N matches no frame-shaped
quantity; installer bookkeeping for the stripped table).

The task brief pointed at the 'SPACER'/'M001' index containers as the frame
source. That turned out to be wrong: those streams (0015.bin, 0016.bin) are
the DeskMate.WAS / DeskMates.Wa3 SOUND archives — 'M001' is a sound entry
name (the M001..M003 voice lines; SPACER the silence placeholder), the
table layout is [u32 total][u32 count][(name_off, data_off)*count][names]
[u32-len-prefixed blocks] as already handled by extract_behaviors.py. They
contain no frames.

The variant frames actually live in the touch masters: touch.FAS holds the
paid-version touch animations (count-form frame table, ids 5600-7051),
touch_demo.FAS the demo ones (ids 5000-5472), and the base poses (id 0
stand, 0003 wait, 0100-0298 sword-training) sit in common_demo.FAS
(ids 0-298). Each variant's own defs (e.g. `maeka_firedance_aa`) reference
those ids directly; the original runtime resolved them against the global
frame table at playback time. All masters share the same 18 DIB palettes
as the variants (verified byte-identical), and the count-form table's
per-frame flag byte selects palette dibs[2*flag] — that is the palette-swap
mechanism.

Reconstruction (provenance: frames verbatim, palettes the variant's own):
take the variant stub bytes untouched, drop the 5-byte stub tail, and
append a count-form frame table `[u32 count][u16 ids * count][u8 flags *
count][block * count]` whose blocks/flags are copied byte-for-byte from the
master files for every frame id the variant's own defs reference directly
(for `maeka_dummy`, whose defs name only shared `common_sword_training_*`
defs, the direct ids of those defs). Base poses included, so each scene is
self-contained.

Usage: python3 analysis/m001_unpack.py <stream_dir> [out_dir]
  stream_dir: ic4_extract.py output for Maeka.exe
  out_dir:    default analysis/maeka_faz (the pack_assets.py maeka source)
Run after maeka.py, before pack_assets.py maeka.
"""
import os, struct, sys, zlib, gzip

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, HERE)
import faz
from faz_convert import parse_reflist

VARIANTS = ('maeka_dark', 'maeka_dummy', 'maeka_firedance', 'maeka_ignius',
            'maeka_knife', 'maeka_rune', 'maeka_splash', 'maeka_strikes',
            'maeka_veridius', 'maeka_white')
BEHAVIORS = os.path.join(ROOT, 'assets', 'characters', 'maeka', 'behaviors')
MASTER_FILES = ('common', 'common_demo', 'common_enter', 'common_enter_demo',
                'touch', 'touch_demo')
ENTRY_KEYS = ('all', 'all_demo', 'all_slow', 'all_slow_demo')


def count_table(d, bs):
    """(ids, flags, data_start) of a count-form frame table, or None."""
    for c in range(len(d) - 4):
        v = struct.unpack_from('<I', d, c)[0]
        if 0 < v <= 4000 and len(d) - (c + 4) == v * (bs + 3):
            ids = list(struct.unpack_from('<%dH' % v, d, c + 4))
            flags = list(d[c + 4 + 2 * v:c + 4 + 3 * v])
            return ids, flags, c + 4 + 3 * v
    return None


def main():
    src = sys.argv[1]
    out = sys.argv[2] if len(sys.argv) > 2 else os.path.join(HERE, 'maeka_faz')
    os.makedirs(out, exist_ok=True)

    # Master frame tables (repo copies are the installer bytes, gzipped).
    masters = {}          # name -> {id: (block, flag)}
    master_defs = {}      # def name -> body (first file wins, like the runtime)
    for m in MASTER_FILES:
        p = os.path.join(BEHAVIORS, m + '.fas.gz')
        d = gzip.open(p, 'rb').read()
        f = faz.parse_faz(d)
        w, h = f['dims']
        bs = (((w * f['bpp'] // 8) + 3) & ~3) * h
        tab = {}
        ct = count_table(d, bs)
        if ct:
            ids, flags, ds = ct
            for k, (i, fl) in enumerate(zip(ids, flags)):
                tab[i] = (d[ds + k * bs: ds + (k + 1) * bs], fl)
        masters[m] = tab
        pr = f['pairs']
        for i in range(0, len(pr) - 1, 2):
            master_defs.setdefault(pr[i], pr[i + 1])

    n = 0
    for f in sorted(os.listdir(src)):
        if not f.endswith('.bin'):
            continue
        raw = open(os.path.join(src, f), 'rb').read()
        if not (len(raw) > 6 and raw[4:6] in (b'\x78\x9c', b'\x78\xda', b'\x78\x01')):
            continue
        d = zlib.decompress(raw[4:])
        try:
            p = faz.parse_faz(d)
        except Exception:
            continue
        name = p['name']
        if name not in VARIANTS or p['frames']:
            continue
        w, h = p['dims']
        bs = (((w * p['bpp'] // 8) + 3) & ~3) * h
        pr = p['pairs']
        vdefs = {pr[i]: pr[i + 1] for i in range(0, len(pr) - 1, 2)}
        own = [k for k in vdefs if k not in ENTRY_KEYS]

        def direct(names, defs):
            ids = set()
            for nm in names:
                for t in parse_reflist(defs.get(nm, '')):
                    if 'id' in t:
                        ids.add(t['id'])
            return ids

        ids = direct(own, vdefs)
        if not ids:
            # maeka_dummy: pure behavior reuse — take the direct ids of the
            # shared defs its entry points name.
            named = {t['name'] for t in parse_reflist(vdefs.get('all', ''))
                     if 'name' in t and t['name'] in master_defs}
            ids = direct(named, master_defs)
        assert ids, name

        # Splice: stub bytes minus the 5-byte table stub, then the table.
        assert len(d) >= 5 and d[-1] == 0, name
        body = d[:-5]
        ids = sorted(ids)
        blocks, flags = [], []
        for i in ids:
            for m in MASTER_FILES:
                if i in masters[m]:
                    blk, fl = masters[m][i]
                    blocks.append(blk)
                    flags.append(fl)
                    break
            else:
                raise SystemExit(f'{name}: frame {i} not in any master')
        table = struct.pack('<I', len(ids)) + \
            struct.pack('<%dH' % len(ids), *ids) + bytes(flags) + \
            b''.join(blocks)
        full = body + table
        open(os.path.join(out, name + '.bin'), 'wb').write(full)
        print(f'{f} -> {name} {len(ids)} frames spliced '
              f'({len(full)} bytes)')
        n += 1
    print(f'{src}: {n} variant scenes -> {out}/')
    assert n == len(VARIANTS), f'expected {len(VARIANTS)}, got {n}'


if __name__ == '__main__':
    main()
