"""Generate a deterministic dictzip fixture with zlib full-flush chunk boundaries."""
from pathlib import Path
import gzip
import struct
import zlib

root = Path(__file__).resolve().parent.parent / "tests/fixtures/dictionary"
entries = [("alpha", "naïve " * 4000), ("beta", "b" * 60000), ("café", "A small café.")]
data = bytearray()
idx = bytearray()
for word, text in entries:
    encoded = text.encode()
    idx += word.encode() + b"\0" + struct.pack(">II", len(data), len(encoded))
    data += encoded
chunk_size = 40000
compressor = zlib.compressobj(level=6, wbits=-15)
chunks = [compressor.compress(data[i:i + chunk_size]) + compressor.flush(zlib.Z_FULL_FLUSH)
          for i in range(0, len(data), chunk_size)]
finish = compressor.flush(zlib.Z_FINISH)
table = struct.pack("<HHH", 1, chunk_size, len(chunks))
table += b"".join(struct.pack("<H", len(chunk)) for chunk in chunks)
extra = b"RA" + struct.pack("<H", len(table)) + table
header = b"\x1f\x8b\x08\x0c" + b"\0" * 4 + b"\0\x03" + struct.pack("<H", len(extra))
compressed = header + extra + b"compressed.dict\0" + b"".join(chunks) + finish
compressed += struct.pack("<II", zlib.crc32(data), len(data))
assert gzip.decompress(compressed) == data
(root / "compressed.dict.dz").write_bytes(compressed)
(root / "compressed.idx").write_bytes(idx)
(root / "compressed.ifo").write_text(
    "StarDict's dict ifo file\nversion=2.4.2\nbookname=Compressed fixture\n"
    f"wordcount={len(entries)}\nidxfilesize={len(idx)}\nsametypesequence=m\n")
