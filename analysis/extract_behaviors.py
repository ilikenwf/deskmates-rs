#!/usr/bin/env python3
"""Extract behavior scripts (FAS) and sound archives (WAS) from the original
Oska installers into each repo's asset tree.

For every installer we unpack the IC4/IC5 stream container, locate the
manifest stream (the text file listing `NAME.FAZ,...` rows), then pull every
`.FAS` script and the `.WAS` wave archive by matching manifest sizes against
the decompressed stream sizes. FAS files are stored gzip-compressed under
`assets/characters/<c>/behaviors/`; WAS sounds are split into individual
8 kHz 16-bit mono WAVs under `assets/characters/<c>/sounds/`.

Usage: python3 analysis/extract_behaviors.py [character ...]   (default: all)
"""
import os, re, struct, subprocess, sys, tempfile, zlib, gzip

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)          # deskmates-rs/
REPOS = os.path.dirname(ROOT)         # ~/repos
INSTALLERS = os.path.expanduser('~/reference/oska-deskmates/installers')

# character -> (installer, repo dir name)
CHARS = {
    'tahni':   ('Tahni_Setup.exe', 'deskmates-rs'),
    'fatb':    ('DeskMates (basic) +FatB.exe', 'deskmates-rs'),
    'maeka':   ('Maeka.exe', 'deskmates-rs'),
    'kahli':   ('Kahli_Setup.exe', 'deskmates-rs'),
    'tavia':   ('Tavia_Setup.exe', 'tavia-rs'),
    'dm05':    ('DM05_Setup.exe', 'dm05-rs'),
    'fantasy': ('Fantasy.exe', 'fantasy-rs'),
    'sirens':  ('Sirens_Setup.exe', 'sirens-rs'),
    'sns':     ('SnS.exe', 'sugar-n-spice-rs'),
    'dancers': ('Dancers_Setup.exe', 'dancing-girls-rs'),
    'arianna': ('Arianna_Setup.exe', 'arianna-rs'),
}

ZM = (b'\x78\x9c', b'\x78\xda', b'\x78\x01')


def unwrap(raw):
    if len(raw) > 4 and raw[4:6] in ZM:
        try:
            return zlib.decompress(raw[4:])
        except zlib.error:
            pass
    if raw[:2] in ZM:
        try:
            return zlib.decompress(raw)
        except zlib.error:
            pass
    return raw


def streams(exe, tmp):
    for tool in ('ic4_extract.py', 'ic5_extract.py'):
        out = os.path.join(tmp, tool[:3])
        r = subprocess.run([sys.executable, os.path.join(HERE, tool), exe, out],
                           capture_output=True, text=True)
        if r.returncode == 0 and os.path.isdir(out) and os.listdir(out):
            return out
    raise RuntimeError(f'no extractor worked for {exe}')


def wav_wrap(pcm):
    return (b'RIFF' + struct.pack('<I', 36 + len(pcm)) + b'WAVEfmt '
            + struct.pack('<IHHIIHH', 16, 1, 1, 8000, 16000, 2, 16)
            + b'data' + struct.pack('<I', len(pcm)) + pcm)


def split_was(data):
    total, count = struct.unpack_from('<II', data, 0)
    base = 8 + count * 8
    out = {}
    for i in range(count):
        no, do = struct.unpack_from('<II', data, 8 + i * 8)
        name = data[base + no:data.index(b'\x00', base + no)].decode('ascii', 'replace')
        dl = struct.unpack_from('<I', data, base + do)[0]
        out[name] = wav_wrap(data[base + do + 4:base + do + 4 + dl])
    return out


def extract(char):
    exe_name, repo = CHARS[char]
    exe = os.path.join(INSTALLERS, exe_name)
    with tempfile.TemporaryDirectory() as tmp:
        sdir = streams(exe, tmp)
        files = sorted(os.listdir(sdir))
        blobs = {f: unwrap(open(os.path.join(sdir, f), 'rb').read()) for f in files}
        manifest = next((b for b in blobs.values()
                         if b[:64].isascii() and b'.FAZ,' in b[:4096]), None)
        if manifest is None:
            print(f'{char}: no manifest found'); return
        want = {}
        for line in manifest.decode('latin1').splitlines():
            parts = line.split(',')
            if len(parts) < 5:
                continue
            fname = parts[0]
            if fname.upper().endswith(('.FAS', '.WAS')):
                try:
                    want[fname] = int(parts[3])
                except ValueError:
                    pass
        beh = os.path.join(REPOS, repo, 'assets/characters', char, 'behaviors')
        snd = os.path.join(REPOS, repo, 'assets/characters', char, 'sounds')
        got = set()
        for fname, rawsz in want.items():
            for f, data in blobs.items():
                if len(data) == rawsz:
                    got.add(fname)
                    if fname.upper().endswith('.WAS'):
                        os.makedirs(snd, exist_ok=True)
                        sounds = split_was(data)
                        for n, wav in sounds.items():
                            with gzip.open(os.path.join(snd, n.upper() + '.wav.gz'), 'wb', 6) as fh:
                                fh.write(wav)
                        print(f'{char}: {fname} -> {len(sounds)} sounds')
                    else:
                        os.makedirs(beh, exist_ok=True)
                        with gzip.open(os.path.join(beh, fname.lower() + '.gz'), 'wb', 6) as fh:
                            fh.write(data)
                        print(f'{char}: {fname} ({len(data)} bytes)')
                    break
            else:
                print(f'{char}: {fname} stream not found (size {rawsz})')


if __name__ == '__main__':
    todo = sys.argv[1:] or list(CHARS)
    for c in todo:
        extract(c)
