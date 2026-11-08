"""Offline fixtures; no device or real flash backup is used."""
import hashlib
import importlib.util
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts"))
SPEC = importlib.util.spec_from_file_location("picco_plan", ROOT / "scripts/picco-update-plan.py")
planner = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(planner)


def image():
    header = bytearray(24)
    header[0:2] = bytes([0xE9, 1])
    header[12:14] = struct.pack("<H", 9)
    header[23] = 1
    body = struct.pack("<I", 0xABCD5432) + bytes(252)
    raw = header + struct.pack("<II", 0x3C000020, len(body)) + body
    checksum = 0xEF
    for byte in body:
        checksum ^= byte
    raw += bytes((15 - len(raw) % 16) % 16) + bytes([checksum])
    return bytes(raw + hashlib.sha256(raw).digest())


ROWS = [
    ("nvs", 1, 2, 0x9000, 0xC000, 0),
    ("otadata", 1, 0, 0x15000, 0x2000, 0),
    ("safeboot", 0, 0, 0x20000, 0xC0000, 0),
    ("app", 0, 0x10, 0xE0000, 0xE70000, 0),
    ("spiffs", 1, 0x82, 0xF50000, 0x80000, 0),
    ("coredump", 1, 3, 0xFD0000, 0x10000, 0),
]


def table(rows):
    raw = b"".join(struct.pack("<HBBII16sI", 0x50AA, kind, subtype, offset, size, name.encode(), flags)
                   for name, kind, subtype, offset, size, flags in rows)
    raw += b"\xeb\xeb" + b"\xff" * 14 + hashlib.md5(raw).digest()
    return raw.ljust(4096, b"\xff")


def backup(rows=ROWS):
    raw = bytearray(b"\xff" * planner.FLASH_BYTES)
    raw[0x8000:0x9000] = table(rows)
    for offset in [0, 0x20000, 0xE0000]:
        raw[offset:offset + len(image())] = image()
    return raw


def manifest(candidate):
    return {"image": "femto-picco.bin", "chip": "esp32s3", "bytes": len(candidate),
            "sha256": planner.sha(candidate), "reference_app_offset": "0xE0000", "reference_app_size": "0xE70000"}


class UpdatePlanTests(unittest.TestCase):
    def test_readback_covers_entire_flash_and_reports_only_offsets_and_hashes(self):
        raw, candidate = backup(), image()
        start, end = 0xE0000, 0xE1000
        written = raw.copy()
        written[start:end] = candidate + b"\xff" * (end - start - len(candidate))
        result = planner.verify_readback(raw, candidate, manifest(candidate), written)
        self.assertEqual(result["kind"], "offline-picco-readback-verification")
        self.assertTrue(result["readback"]["matches_plan"])
        self.assertFalse(result["hardware_verified"])
        self.assertFalse(result["write_authorized"])
        self.assertEqual(sum(check["bytes"] for check in result["readback"]["checks"]), len(raw))
        for offset, name in [(0, "preserved_prefix"), (start - 1, "preserved_prefix"),
                             (start, "candidate"), (start + len(candidate) - 1, "candidate"),
                             (start + len(candidate), "erased_tail"), (end - 1, "erased_tail"),
                             (end, "preserved_suffix"), (len(raw) - 1, "preserved_suffix")]:
            with self.subTest(offset=offset):
                corrupt = written.copy()
                corrupt[offset] ^= 1
                result = planner.verify_readback(raw, candidate, manifest(candidate), corrupt)
                self.assertFalse(result["readback"]["matches_plan"])
                failed = [check for check in result["readback"]["checks"] if not check["matches"]]
                self.assertEqual(len(failed), 1)
                self.assertEqual(failed[0]["name"], name)
                self.assertEqual(failed[0]["first_mismatch_offset"], offset)
        for invalid in [written[:-1], written + b"\xff"]:
            with self.assertRaisesRegex(ValueError, "readback must contain exactly"):
                planner.verify_readback(raw, candidate, manifest(candidate), invalid)
        wrong = manifest(candidate)
        wrong["sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "manifest mismatch"):
            planner.verify_readback(raw, candidate, wrong, written)

    def test_valid_plan_preserves_recovery_and_defers_hardware_authorization(self):
        raw, candidate = backup(), image()
        before = planner.sha(raw)
        result = planner.plan(raw, candidate, manifest(candidate))
        self.assertEqual(planner.sha(raw), before)
        self.assertFalse(result["write_authorized"])
        self.assertFalse(result["hardware_verified"])
        self.assertEqual(result["proposed_write"]["offset"], 0xE0000)
        self.assertEqual(result["proposed_erase"]["bytes"], 4096)
        self.assertEqual(result["preserve"][0]["bytes"], 0xE0000)
        self.assertEqual(result["preserve"][1]["offset"], 0xE1000)
        self.assertEqual(result["rollback"]["sha256"], planner.sha(raw[0xE0000:0xF50000]))

    def test_image_checks_detect_corruption_and_unsupported_content(self):
        for offset in [0, 12, 32, len(image()) - 1]:
            bad = bytearray(image())
            bad[offset] ^= 1
            with self.assertRaises(ValueError):
                planner.image_info(bad, exact=True)
        for bad in [image()[:30], image() + b"signed-or-garbage"]:
            with self.assertRaises(ValueError):
                planner.image_info(bad, exact=True)
        bad = bytearray(image())
        struct.pack_into("<I", bad, 28, 0xFFFFFFFF)
        with self.assertRaisesRegex(ValueError, "truncated image segment"):
            planner.image_info(bad, exact=True)

    def test_partition_validation_checks_checksum_overlap_flags_and_profile(self):
        variants = []
        for index, replacement in [
            (0, ("nvs", 1, 2, 0x10000, 0xC000, 0)),
            (0, ("nvs", 1, 2, 0x9000, 0xC000, 1)),
            (2, ("safeboot", 0, 0x10, 0x20000, 0xC0000, 0)),
            (3, ("app", 0, 0x10, 0xE0000, 0xE60000, 0)),
            (4, ("nvs", 1, 0x82, 0xF50000, 0x80000, 0)),
        ]:
            rows = ROWS.copy()
            rows[index] = replacement
            variants.append(backup(rows))
        bad = backup()
        bad[0x8004] ^= 1
        variants.append(bad)
        for raw in variants:
            with self.assertRaises(ValueError):
                planner.plan(raw, image(), manifest(image()))

    def test_backup_images_and_manifest_are_required(self):
        for offset in [32, 0x20000 + 32, 0xE0000 + 32]:
            bad = backup()
            bad[offset] ^= 1
            with self.assertRaises(ValueError):
                planner.plan(bad, image(), manifest(image()))
        with self.assertRaises(ValueError):
            planner.plan(backup()[:-1], image(), manifest(image()))
        wrong = manifest(image())
        wrong["sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "manifest mismatch"):
            planner.plan(backup(), image(), wrong)

    def test_packaging_checks_full_image_before_replacing_manifest(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "femto-picco.bin"
            path.write_bytes(image())
            command = [sys.executable, str(ROOT / "scripts/picco-image-manifest.py"), str(path), "2"]
            subprocess.run(command, check=True, capture_output=True)
            saved = path.with_suffix(".json").read_bytes()
            self.assertEqual(json.loads(saved)["sha256"], planner.sha(image()))
            corrupt = bytearray(image())
            corrupt[-1] ^= 1
            path.write_bytes(corrupt)
            result = subprocess.run(command, capture_output=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(path.with_suffix(".json").read_bytes(), saved)

    def test_cli_emits_plan_without_mutating_inputs(self):
        with tempfile.TemporaryDirectory() as folder:
            folder = Path(folder)
            files = {"backup.bin": bytes(backup()), "femto-picco.bin": image(),
                     "manifest.json": json.dumps(manifest(image())).encode()}
            for name, data in files.items():
                (folder / name).write_bytes(data)
            command = [sys.executable, str(ROOT / "scripts/picco-update-plan.py"),
                       "--backup", str(folder / "backup.bin"), "--image", str(folder / "femto-picco.bin"),
                       "--manifest", str(folder / "manifest.json")]
            result = subprocess.run(command, capture_output=True, check=True)
            self.assertEqual(json.loads(result.stdout)["kind"], "offline-picco-update-plan")
            self.assertEqual(set(p.name for p in folder.iterdir()), set(files))
            for name, data in files.items():
                self.assertEqual((folder / name).read_bytes(), data)
            readback = bytearray(files["backup.bin"])
            readback[0xE0000:0xE1000] = image().ljust(4096, b"\xff")
            readback_path = folder / "readback.bin"
            readback_path.write_bytes(readback)
            verify = command + ["--readback", str(readback_path)]
            result = subprocess.run(verify, capture_output=True, check=True)
            self.assertTrue(json.loads(result.stdout)["readback"]["matches_plan"])
            readback[0xE0000] ^= 1
            readback_path.write_bytes(readback)
            result = subprocess.run(verify, capture_output=True)
            self.assertEqual(result.returncode, 1)
            self.assertFalse(json.loads(result.stdout)["readback"]["matches_plan"])
            self.assertEqual(readback_path.read_bytes(), readback)
            for name, data in files.items():
                self.assertEqual((folder / name).read_bytes(), data)
            (folder / "manifest.json").write_text("[]")
            failed = subprocess.run(command, capture_output=True)
            self.assertEqual(failed.returncode, 1)
            self.assertEqual(failed.stdout, b"")


if __name__ == "__main__":
    unittest.main()
