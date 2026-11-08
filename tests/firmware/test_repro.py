import importlib.util
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "picco_repro", Path(__file__).resolve().parents[2] / "scripts/picco-repro.py")
repro = importlib.util.module_from_spec(spec)
spec.loader.exec_module(repro)


class ReproTests(unittest.TestCase):
    def test_explicit_variant_overrides_ambient_sdk_configuration(self):
        inherited = {"FEMTO_PICCO_BLUETOOTH": "1", "FEMTO_PICCO_GRAYSCALE": "1",
                     "ESP_IDF_SDKCONFIG": "foreign", "ESP_IDF_SDKCONFIG_DEFAULTS": "foreign",
                     "FEMTO_IDF_SHELL": "1", "FEMTO_PICCO_BUILD_NAMESPACE": "1"}
        with patch.dict(os.environ, inherited):
            for bluetooth in (False, True):
                for levels in (2, 4):
                    env = repro.build_environment(Path("fresh"), Path("artifacts"), levels, bluetooth)
                    self.assertEqual(env["FEMTO_PICCO_BLUETOOTH"], str(int(bluetooth)))
                    self.assertEqual(env["FEMTO_PICCO_GRAYSCALE"], str(int(levels == 4)))
                    self.assertEqual(env["CARGO_TARGET_DIR"], "fresh")
                    self.assertEqual(env["CARGO_BUILD_JOBS"], "1")
                    for key in inherited.keys() - {"FEMTO_PICCO_BLUETOOTH", "FEMTO_PICCO_GRAYSCALE"}:
                        self.assertNotIn(key, env)

    def test_input_inventory_catches_add_remove_change_and_ignores_build_outputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / ".gitignore").write_text("/target/\n")
            source = root / "source.rs"
            source.write_text("original")
            expected = repro.source_inputs(root)
            (root / "target").mkdir()
            (root / "target/generated").write_text("ignored without a .git directory")
            repro.verify_inputs(root, expected)
            extra = root / "added.rs"
            extra.write_text("new")
            with self.assertRaisesRegex(RuntimeError, "added=.*added.rs"):
                repro.verify_inputs(root, expected)
            extra.unlink()
            source.write_text("changed")
            with self.assertRaisesRegex(RuntimeError, "changed=.*source.rs"):
                repro.verify_inputs(root, expected)
            source.unlink()
            with self.assertRaisesRegex(RuntimeError, "removed=.*source.rs"):
                repro.verify_inputs(root, expected)

    def test_source_symlinks_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "original").write_text("content")
            (root / "link").symlink_to("original")
            with self.assertRaisesRegex(RuntimeError, "symlink"):
                repro.source_inputs(root)

    def test_source_directory_symlinks_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "original").mkdir()
            (root / "original/file.rs").write_text("content")
            (root / "link").symlink_to("original", target_is_directory=True)
            with self.assertRaisesRegex(RuntimeError, "symlink"):
                repro.source_inputs(root)
