#!/usr/bin/env python3
"""Extract behavior scripts (FAS) and sound archives (WAS) from the original
Oska installers into each repo's asset tree.

For every installer we unpack the IC4/IC5 stream container, locate the
manifest stream (the text file listing `NAME.FAZ,...` rows), then pull every
`.FAS` script and the `.WAS` wave archive by matching manifest sizes against
the decompressed stream sizes. FAS files are stored gzip-compressed under
`assets/characters/<c>/behaviors/`; WAS sounds are split into individual
8 kHz 16-bit mono WAVs under `assets/characters/<c>/sounds/`.

Newer installers (Maeka.exe) ship their sounds in a second archive,
`DeskMates.Wa3`, whose `DeskMate.WAS` sibling holds only the SPACER silence
placeholder. The Wa3 table layout is identical to WAS — [u32 total][u32
count][(name_off, data_off)*count][names][u32-len-prefixed blocks] — but each
block is a raw MPEG-2 Layer III stream (22.05 kHz mono, 32 kbps, no RIFF
header) instead of raw PCM, so blocks are decoded to WAV with ffmpeg. The
entry names (M001.., T001..) are exactly the ` !name` sound tokens the
character's scene/FAS scripts reference (email.FAZ -> !M001..M003,
touch.FAS -> !T001..T011).

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


def wav_wrap(pcm, rate=8000):
    return (b'RIFF' + struct.pack('<I', 36 + len(pcm)) + b'WAVEfmt '
            + struct.pack('<IHHIIHH', 16, 1, 1, rate, rate * 2, 2, 16)
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


def split_wa3(data):
    """DeskMates.Wa3: same table as split_was, but each block is a raw
    MPEG-2 Layer III stream (22.05 kHz mono, 32 kbps) instead of raw PCM —
    decode each block to PCM with ffmpeg and wrap it as a WAV."""
    total, count = struct.unpack_from('<II', data, 0)
    base = 8 + count * 8
    out = {}
    for i in range(count):
        no, do = struct.unpack_from('<II', data, 8 + i * 8)
        name = data[base + no:data.index(b'\x00', base + no)].decode('ascii', 'replace')
        dl = struct.unpack_from('<I', data, base + do)[0]
        p = subprocess.run(
            ['ffmpeg', '-loglevel', 'error', '-f', 'mp3', '-i', 'pipe:0',
             '-ac', '1', '-c:a', 'pcm_s16le', '-f', 's16le', 'pipe:1'],
            input=data[base + do + 4:base + do + 4 + dl], capture_output=True)
        if p.returncode != 0:
            raise RuntimeError(f'ffmpeg failed on {name}: {p.stderr[-200:]}')
        out[name] = wav_wrap(p.stdout, 22050)
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
            if fname.upper().endswith(('.FAS', '.WAS', '.WA3')):
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
                    if fname.upper().endswith(('.WAS', '.WA3')):
                        os.makedirs(snd, exist_ok=True)
                        splitter = split_wa3 if fname.upper().endswith('.WA3') else split_was
                        sounds = splitter(data)
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


def extract_tree(char):
    """Recovered (already-unpacked) tree from the archive.org archaeology
    (cdx-findings/extracted/*_tree) -> repo asset layout:
    *.FAZ -> scenes/<name>.bin.gz, *.FAS -> behaviors/<name>.fas.gz,
    DeskMate.WAS / DeskMates.Wa3 -> sounds/*.wav.gz."""
    tdir = os.path.join(CDX, TREES[char])
    out = os.path.join(REPOS, 'deskmates-rs', 'assets/characters', char)
    n_sc = n_be = n_sn = 0
    for f in sorted(os.listdir(tdir)):
        src = os.path.join(tdir, f)
        up = f.upper()
        if up.endswith('.FAZ'):
            dst = os.path.join(out, 'scenes', f.lower()[:-4] + '.bin.gz')
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            with open(src, 'rb') as fh, gzip.open(dst, 'wb', 6) as g:
                g.write(fh.read())
            n_sc += 1
        elif up.endswith('.FAS'):
            dst = os.path.join(out, 'behaviors', f.lower()[:-4] + '.fas.gz')
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            with open(src, 'rb') as fh, gzip.open(dst, 'wb', 6) as g:
                g.write(fh.read())
            n_be += 1
        elif up in ('DESKMATE.WAS', 'DESKMATES.WA3'):
            data = open(src, 'rb').read()
            splitter = split_wa3 if up.endswith('WA3') else split_was
            sounds = splitter(data)
            sdir = os.path.join(out, 'sounds')
            os.makedirs(sdir, exist_ok=True)
            for n, wav in sounds.items():
                with gzip.open(os.path.join(sdir, n.upper() + '.wav.gz'), 'wb', 6) as g:
                    g.write(wav)
            n_sn += len(sounds)
    print(f'{char}: {n_sc} scenes, {n_be} behaviors, {n_sn} sounds')


CDX = os.path.expanduser('~/reference/oska-deskmates/cdx-findings/extracted')
TREES = {
    'teecee': 'TeeCee_tree',
    'johlee': 'Johlee_tree',
    'oska':   'Oska_tree',
}


if __name__ == '__main__':
    argv = sys.argv[1:]
    if argv and argv[0] == '--tree':
        for c in argv[1:]:
            extract_tree(c)
    else:
        for c in (argv or list(CHARS)):
            extract(c)
