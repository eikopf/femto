"""Check a local WikDict StarDict corpus with an independent binary index reader."""
from pathlib import Path
import collections
import gzip
import hashlib
import json
import shutil
import struct
import subprocess
import sys
import zlib

root = Path(sys.argv[1]).resolve()
base = root / "stardict"
info = dict(line.split("=", 1) for line in base.with_suffix(".ifo").read_text().splitlines()[1:] if "=" in line)
assert info["sametypesequence"] == "h"
assert info.get("idxoffsetbits", "32") == "32"
raw = base.with_suffix(".dict").read_bytes()
index = base.with_suffix(".idx").read_bytes()
assert len(index) == int(info["idxfilesize"])
entries = []
pos = 0
while pos < len(index):
    end = index.index(0, pos)
    word = index[pos:end].decode()
    offset, size = struct.unpack_from(">II", index, end + 1)
    assert offset + size <= len(raw)
    entries.append((word, offset, size))
    pos = end + 9
assert pos == len(index) and len(entries) == int(info["wordcount"])
fold = lambda word: word.encode().lower()
assert entries == sorted(entries, key=lambda e: (fold(e[0]), e[0].encode()))
heads = collections.defaultdict(list)
for entry in entries:
    heads[fold(entry[0])].append(entry)
index = base.with_suffix(".syn").read_bytes()
synonyms = []
pos = 0
while pos < len(index):
    end = index.index(0, pos)
    word = index[pos:end].decode()
    number, = struct.unpack_from(">I", index, end + 1)
    assert number < len(entries)
    synonyms.append((word, number))
    pos = end + 5
assert pos == len(index) and len(synonyms) == int(info["synwordcount"])
assert synonyms == sorted(synonyms, key=lambda e: (fold(e[0]), e[0].encode()))
syns = collections.defaultdict(list)
for word, number in synonyms:
    syns[fold(word)].append(number)

def expected(query):
    found = list(heads[fold(query)][:8])
    for number in syns[fold(query)][:8]:
        if len(found) == 8:
            break
        entry = entries[number]
        if not any(e[1] == entry[1] for e in found):
            found.append(entry)
    if not found and query.lower() != query:
        return expected(query.lower())
    return [{"word": word, "raw": raw[offset:offset + size].decode()} for word, offset, size in found]

def sample(values, count):
    return [values[i * (len(values) - 1) // (count - 1)][0] for i in range(min(count, len(values)))]

queries = sample(entries, 4096) + sample(synonyms, 512)
queries += [e[0] for e in sorted(entries, key=lambda e: e[2])[-8:]]
queries += [q.upper() for q in queries[:64]] + ["femto_nonexistent_corpus_probe_742"]
queries = list(dict.fromkeys(queries))
cases = root / "cases.jsonl"
with cases.open("w") as out:
    for query in queries:
        out.write(json.dumps({"query": query, "expected": expected(query)}, ensure_ascii=False) + "\n")

compressed = root / "compressed"
compressed.mkdir(exist_ok=True)
for extension in ["ifo", "idx", "syn"]:
    shutil.copyfile(base.with_suffix("." + extension), compressed / ("stardict." + extension))
size = 58315
encoder = zlib.compressobj(level=6, wbits=-15)
chunks = [encoder.compress(raw[i:i + size]) + encoder.flush(zlib.Z_FULL_FLUSH) for i in range(0, len(raw), size)]
finish = encoder.flush(zlib.Z_FINISH)
table = struct.pack("<HHH", 1, size, len(chunks)) + b"".join(struct.pack("<H", len(c)) for c in chunks)
extra = b"RA" + struct.pack("<H", len(table)) + table
archive = b"\x1f\x8b\x08\x04" + b"\0" * 5 + b"\x03" + struct.pack("<H", len(extra)) + extra
archive += b"".join(chunks) + finish + struct.pack("<II", zlib.crc32(raw), len(raw))
assert gzip.decompress(archive) == raw
(compressed / "stardict.dict.dz").write_bytes(archive)
reports = {}
for name, path in [("published_plain", base.with_suffix(".ifo")), ("derived_dictzip", compressed / "stardict.ifo")]:
    result = subprocess.run(["target/debug/examples/dictionary-corpus", str(path), str(cases)], capture_output=True, text=True, timeout=240)
    if result.returncode:
        raise RuntimeError(f"{name} probe failed: {result.stderr}")
    reports[name] = json.loads(result.stdout)
    print(name, result.stdout.strip(), flush=True)
report = {"source": "https://download.wikdict.com/dictionaries/stardict/wikdict-en-de.zip", "date": info.get("date"), "entries": len(entries), "synonyms": len(synonyms), "sample_queries": len(queries), "reports": reports,
          "sha256": {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in [root / "wikdict-en-de.zip", base.with_suffix(".ifo"), base.with_suffix(".idx"), base.with_suffix(".dict"), base.with_suffix(".syn"), cases, compressed / "stardict.dict.dz"]}}
(root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
