"""Describe the app image and reference slot; never emits a flash command."""
import argparse
import hashlib
import json
from pathlib import Path

from picco_image import image_info

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("image", type=Path)
parser.add_argument("levels", type=int, choices=(2, 4), nargs="?", default=2)
parser.add_argument("--bluetooth", action="store_true", help="record Bluetooth support in this build")
args = parser.parse_args()
path = args.image
levels = args.levels
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
    "bluetooth": args.bluetooth,
    "reference_app_offset": "0xE0000",
    "reference_app_size": "0xE70000",
    "hardware_verified": False,
    "installation_status": "No automatic installer. Verify the actual device partition table and recovery backup before installation.",
}
path.with_suffix(".json").write_text(json.dumps(manifest, indent=2) + "\n")
print(json.dumps(manifest, indent=2))
