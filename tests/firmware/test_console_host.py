"""Exercise the host console over a pseudo-terminal without a device."""
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]


class ConsoleHostTests(unittest.TestCase):
    def exchange(self, response, expected=1):
        master, slave = os.openpty()
        try:
            with subprocess.Popen(
                [sys.executable, str(ROOT / "scripts/picco-console.py"),
                 "--port", os.ttyname(slave), "--expect-page", str(expected),
                 "--timeout", "2", "next"], stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                text=True,
            ) as process:
                self.assertTrue(select.select([master], [], [], 2)[0])
                commands = bytearray()
                while commands.count(b"\n") < 2:
                    self.assertTrue(select.select([master], [], [], 2)[0])
                    commands.extend(os.read(master, 1024))
                self.assertEqual(commands, b"femto next\nfemto status\n")
                os.write(master, response)
                stdout, stderr = process.communicate(timeout=4)
                return process.returncode, stdout, stderr
        finally:
            os.close(master)
            os.close(slave)

    def test_status_and_memory_are_parsed_after_navigation(self):
        code, output, error = self.exchange(
            b"I (1) femto: Stack usb status: minimum_free=28000\r\n"
            b"I (1) femto: Heap usb status psram: free=7900000 largest=7800000 minimum=7500000\r\n"
            b"I (1) femto: USB status: page=1 pages=4 menu=false\r\n")
        self.assertEqual(code, 0, error)
        status = json.loads(output)
        self.assertEqual((status["page"], status["menu"]), (1, False))
        self.assertEqual(status["memory_bytes"]["stack_minimum_free"], 28000)
        self.assertEqual(status["memory_bytes"]["psram"]["free"], 7900000)

    def test_unexpected_page_fails(self):
        code, _, _ = self.exchange(b"USB status: page=0 pages=4 menu=false\n")
        self.assertEqual(code, 1)

    def test_overlong_output_is_rejected(self):
        code, _, error = self.exchange(b"x" * 4097)
        self.assertNotEqual(code, 0)
        self.assertIn("USB line exceeds 4096 bytes", error)
