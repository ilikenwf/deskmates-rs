#!/usr/bin/env python3
"""Extract all files from a ClickTeam Install Creator V5 installer.

Payload: after PE sections -> 6-byte magic, entries [u32 id][u32 comp][u32 raw][u8 meth].
The entry with id 0x7f7f wraps the real file region: a sequence of concatenated
zlib streams; each stream = u32 len + zlib(file bytes).
"""
import struct, sys, zlib, os

def pe_end(d):
    e = struct.unpack_from('<I', d, 0x3c)[0]
    nsec = struct.unpack_from('<H', d, e + 6)[0]
    optsz = struct.unpack_from('<H', d, e + 20)[0]
    sec = e + 24 + optsz
    end = 0
    for i in range(nsec):
        end = max(end, struct.unpack_from('<I', d, sec + 40*i + 16)[0] +
                       struct.unpack_from('<I', d, sec + 40*i + 20)[0])
    return end

def extract(src, outdir):
    d = open(src, 'rb').read()
    app = d[pe_end(d):]
    # walk outer entries to find the big file region (id 0x7f7f)
    pos = 6
    region = None
    while pos + 13 <= len(app):
        eid, comp, raw, meth = struct.unpack_from('<IIIB', app, pos)
        pos += 13
        if eid == 0x7f7f:
            region = app[pos:pos+comp]
            break
        # skip this entry's stream
        blob = app[pos:pos+comp]
        if meth == 1:
            z = zlib.decompressobj()
            try:
                z.decompress(blob)
                pos += len(blob) - len(z.unused_data)
            except Exception:
                pos += comp
        else:
            pos += comp
    if region is None:
        print('no 0x7f7f region found'); return
    os.makedirs(outdir, exist_ok=True)
    p = 0
    n = 0
    while p < len(region) - 2:
        if region[p] == 0x78:
            try:
                z = zlib.decompressobj()
                out = z.decompress(region[p:])
                consumed = len(region) - p - len(z.unused_data)
                p += consumed
                if n >= 2:
                    try:
                        out = zlib.decompress(out[4:])
                    except zlib.error:
                        pass
                open(os.path.join(outdir, f'{n:04d}.bin'), 'wb').write(out)
                n += 1
                continue
            except zlib.error:
                pass
        p += 1
    print(f'{n} files -> {outdir}')

if __name__ == '__main__':
    extract(sys.argv[1], sys.argv[2])
