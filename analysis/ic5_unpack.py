#!/usr/bin/env python3
"""Unpacker for ClickTeam Install Creator V5 installers (Tahni_Setup.exe et al.).

Appended payload layout (after PE sections):
  6-byte magic, then a sequence of entries:
    u32 id, u32 comp_size, u32 raw_size, u8 method, data[comp_size]
  method 1 = zlib, 2 = bzip2, 0 = stored.
"""
import struct, sys, zlib, bz2, os, re

def entries(path):
    d = open(path, 'rb').read()
    # find PE end
    e_lfanew = struct.unpack_from('<I', d, 0x3c)[0]
    nsec = struct.unpack_from('<H', d, e_lfanew + 6)[0]
    optsz = struct.unpack_from('<H', d, e_lfanew + 20)[0]
    sec = e_lfanew + 24 + optsz
    end = 0
    for i in range(nsec):
        end = max(end, struct.unpack_from('<I', d, sec + 40*i + 16)[0] +
                       struct.unpack_from('<I', d, sec + 40*i + 20)[0])
    app = d[end:]
    pos = 6  # magic
    while pos + 13 <= len(app):
        eid, comp, raw, meth = struct.unpack_from('<IIIB', app, pos)
        pos += 13
        blob = app[pos:pos+comp+64]
        consumed = comp
        if meth == 1:
            z = zlib.decompressobj()
            data = z.decompress(blob)
            consumed = len(blob) - len(z.unused_data)
        elif meth == 2:
            b = bz2.BZ2Decompressor()
            data = b.decompress(blob)
            # BZ2Decompressor doesn't expose unused; find stream end by trial
            lo, hi = 10, len(blob)
            while lo < hi:
                mid = (lo + hi) // 2
                try:
                    bz2.decompress(blob[:mid]); hi = mid
                except Exception:
                    lo = mid + 1
            consumed = lo
        else:
            data = blob[:comp]
        pos += consumed
        yield eid, meth, raw, data

if __name__ == '__main__':
    src, outdir = sys.argv[1], sys.argv[2]
    os.makedirs(outdir, exist_ok=True)
    for eid, meth, raw, data in entries(src):
        # sniff type for naming
        kind = 'bin'
        if data[:2] == b'BZh9': kind = 'bz2'
        elif data[:4] == b'MZ': kind = 'exe'
        elif data[:6] in (b'GIF87a', b'GIF89a'): kind = 'gif'
        elif data[:8] == b'\x89PNG\r\n\x1a\n': kind = 'png'
        elif data[:2] == b'BM': kind = 'bmp'
        elif data[:4] == b'RIFF': kind = 'wav'
        elif data[:4] == b'FORM': kind = 'iff'
        elif data[:4] == b'PK\x03\x04': kind = 'zip'
        elif data[:5] == b'%PDF-': kind = 'pdf'
        elif re.match(rb'^[\x09\x0a\x0d\x20-\x7e]{16,}', data): kind = 'txt'
        fn = os.path.join(outdir, f'{eid:08x}_{meth}_{kind}.dat')
        open(fn, 'wb').write(data)
        print(f'{eid:08x} meth={meth} raw={raw:9d} kind={kind}')
    print('done')
