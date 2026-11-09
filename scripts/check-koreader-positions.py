"""Validate EPUB locations against text-node paths from the official engine."""
import json
import hashlib
import os
import re
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parent.parent
os.chdir(root)
books = []
for name in ["sample.epub", "whitespace.epub", "navigation.epub", "lists.epub", "tab-size.epub", "kosync-css.epub"]:
    output = subprocess.check_output(["./scripts/koreader-position-oracle", "tests/fixtures/" + name], text=True)
    nodes = []
    non_body_nodes = []
    for line in output.splitlines():
        if not line.startswith("/body"):
            continue
        path, encoded = line.split("\t")
        node = {"path": path, "text": bytes.fromhex(encoded).decode("utf-8")}
        if re.match(r"^/body\[1\]/DocFragment\[\d+\]/body\[1\]/", path):
            nodes.append(node)
        else:
            non_body_nodes.append(node)
    assert nodes
    books.append({"file": name, "sha256": hashlib.sha256((root / "tests/fixtures" / name).read_bytes()).hexdigest(), "nodes": nodes, "non_body_nodes": non_body_nodes})
report = root / "output/koreader-position-vectors.json"
report.write_text(json.dumps({"engine_revision": "fb4d562a2765c52bff052759112a94a0b05239db", "books": books}, ensure_ascii=False, indent=2) + "\n")
subprocess.run(["cargo", "test", "--locked", "-p", "femto-reader", "--test", "koreader_engine"], check=True,
               env=dict(os.environ, FEMTO_KOREADER_POSITION_FIXTURE=str(report)))

subprocess.run(["cargo", "test", "--locked", "-p", "femto-reader", "--test", "koreader_engine", "--", "--ignored"], check=True)
subprocess.run(["python", "scripts/check-koreader-images.py"], check=True)
