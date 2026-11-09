"""Compare Rust text-node normalization with the pinned KOReader engine."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import zipfile

root = Path(__file__).resolve().parent.parent
os.chdir(root)
cases = [
    {"name": "entities-unicode", "raw": "A  B&#32;  C &amp; 😀é", "preserve": False},
    {"name": "long-word", "raw": "a" * 20000, "preserve": False},
    {"name": "word-boundary-unicode", "raw": "😀" * 8000 + " " + "x" * 500, "preserve": False},
    {"name": "tabs", "raw": "a\t😀\n\tb", "preserve": True},
    {"name": "initial-newline", "raw": "\nA\r\n B", "preserve": True, "strip": True},
    {"name": "cdata", "raw": "a &amp;  b", "preserve": False, "cdata": True},
    {"name": "line-endings", "raw": "a\r\nb\n\rc\rd\ne", "preserve": True},
]
with tempfile.TemporaryDirectory(prefix="femto-koreader-text-") as temporary:
    book = Path(temporary) / "positions.epub"
    body = []
    counts = {}
    for case in cases:
        case.setdefault("cdata", False)
        case.setdefault("strip", False)
        tag = "pre" if case["strip"] else "p"
        counts[tag] = counts.get(tag, 0) + 1
        case["path"] = f"/body[1]/DocFragment[1]/body[1]/{tag}[{counts[tag]}]/text()"
        style = ' style="white-space: pre"' if case["preserve"] else ""
        raw = "<![CDATA[" + case["raw"] + "]]>" if case["cdata"] else case["raw"]
        body.append(f"<{tag}{style}>{raw}</{tag}>")
    xml = '<html xmlns="http://www.w3.org/1999/xhtml"><head><title>Positions</title></head><body>' + "".join(body) + "</body></html>"
    with zipfile.ZipFile("tests/fixtures/sample.epub") as source, zipfile.ZipFile(book, "w") as destination:
        first = next(name for name in source.namelist() if name.endswith(".xhtml") and "nav" not in name)
        for name in source.namelist():
            destination.writestr(name, xml if name == first else source.read(name))
    result = subprocess.check_output(["./scripts/koreader-position-oracle", str(book)], text=True)
    for case in cases:
        case["expected"] = []
        for line in result.splitlines():
            path, encoded = line.split("\t")
            if path.startswith(case["path"] + "["):
                case["expected"].append(bytes.fromhex(encoded).decode("utf-8"))
        assert case["expected"], case["name"]
    report = root / "output/koreader-text-nodes.json"
    report.write_text(json.dumps({"engine_revision": "fb4d562a2765c52bff052759112a94a0b05239db", "cases": cases}, ensure_ascii=False, indent=2) + "\n")
    env = dict(os.environ, FEMTO_KOREADER_TEXT_FIXTURE=str(report))
    subprocess.run(["cargo", "test", "--locked", "-p", "femto-reader", "--test", "koreader_text"], check=True, env=env)
