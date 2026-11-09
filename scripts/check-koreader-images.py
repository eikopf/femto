"""Validate image source positions against the pinned KOReader engine."""
import hashlib
import json
import os
from pathlib import Path
import subprocess

root = Path(__file__).resolve().parent.parent
os.chdir(root)
book = root / "tests/fixtures/kosync-images.epub"
output = subprocess.check_output(["./scripts/koreader-position-oracle", str(book), "--images"], text=True)
images = []
for line in output.splitlines():
    kind, pointer, source = line.split("\t")
    assert kind == "IMAGE"
    images.append({"pointer":pointer, "source":bytes.fromhex(source).decode()})
assert len(images) == 4
report = root / "output/koreader-image-vectors.json"
report.write_text(json.dumps({"engine_revision":"fb4d562a2765c52bff052759112a94a0b05239db", "sha256":hashlib.sha256(book.read_bytes()).hexdigest(), "images":images}, indent=2) + "\n")
env = dict(os.environ, FEMTO_KOREADER_IMAGE_FIXTURE=str(report))
subprocess.run(["cargo", "test", "--locked", "-p", "femto-reader", "--test", "koreader_images"], check=True, env=env)
subprocess.run(["cargo", "test", "--locked", "-p", "femto-reader", "--test", "koreader_images", "--", "--ignored"], check=True, env=env)
