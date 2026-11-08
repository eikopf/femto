"""Describe the app image and reference slot; never emits a flash command."""
import hashlib
import json
from pathlib import Path
import sys

from picco_image import image_info

path = Path(sys.argv[1])
levels = int(sys.argv[2]) if len(sys.argv) > 2 else 2
if levels not in (2, 4):
    raise SystemExit("Display levels must be 2 or 4")
image = path.read_bytes()
try:
    image_info(image, exact=True)
except ValueError as error:
    raise SystemExit(str(error)) from error
if len(image) > 0xE70000:
    raise SystemExit("Image exceeds the reference Picco app slot")
manifest = {
    "image": path.name,
    "sha256": hashlib.sha256(image).hexdigest(),
    "bytes": len(image),
    "chip": "esp32s3",
    "display_levels": levels,
    "reference_app_offset": "0xE0000",
    "reference_app_size": "0xE70000",
    "hardware_verified": False,
    "installation_status": "No automatic installer. Verify the actual device partition table and recovery backup before installation.",
}
path.with_suffix(".json").write_text(json.dumps(manifest, indent=2) + "\n")
print(json.dumps(manifest, indent=2))
