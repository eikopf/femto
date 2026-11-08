"""Firmware variant metadata and offline update-plan compatibility."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from test_update_plan import ROOT, backup, image, planner


class ManifestVariantTests(unittest.TestCase):
    def test_all_variants_preserve_image_hash_and_offline_planner(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "femto-picco.bin"
            candidate = image()
            path.write_bytes(candidate)
            original = bytes(backup())
            for levels in (2, 4):
                for bluetooth in (False, True):
                    command = [sys.executable, str(ROOT / "scripts/picco-image-manifest.py"),
                               str(path), str(levels)] + (["--bluetooth"] if bluetooth else [])
                    result = subprocess.run(command, capture_output=True, check=True)
                    manifest = json.loads(path.with_suffix(".json").read_text())
                    self.assertEqual(json.loads(result.stdout), manifest)
                    self.assertEqual(manifest["display_levels"], levels)
                    self.assertIs(manifest["bluetooth"], bluetooth)
                    self.assertEqual(manifest["sha256"], planner.sha(candidate))
                    self.assertEqual(path.read_bytes(), candidate)
                    self.assertEqual(planner.plan(original, candidate, manifest)["kind"],
                                     "offline-picco-update-plan")
            saved = path.with_suffix(".json").read_bytes()
            for arguments in [["3"], ["2", "--bluetooth=false"], ["2", "unexpected"]]:
                result = subprocess.run([sys.executable, str(ROOT / "scripts/picco-image-manifest.py"),
                                         str(path), *arguments], capture_output=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(path.with_suffix(".json").read_bytes(), saved)
