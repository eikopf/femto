"""Check FAT volume prefixes using the production conversion function."""
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class FatDriveTests(unittest.TestCase):
    def test_volume_paths(self):
        source = (ROOT / "vendor/esp-idf-svc/src/fs/fatfs.rs").read_text()
        start = source.index("    pub(crate) fn drive_path_from")
        end = source.index("    pub(crate) fn drive_path(&self)", start)
        function = source[start:end]
        harness = function + r'''
fn main() {
    for drive in 0..10 {
        let path = drive_path_from(drive);
        let path = unsafe { std::ffi::CStr::from_ptr(path.as_ptr()) };
        assert_eq!(path.to_str().unwrap(), format!("{drive}:"));
    }
}
'''
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / "drive.rs"
            binary = root / "drive"
            path.write_text(harness)
            subprocess.run(["rustc", "--edition=2021", str(path), "-o", str(binary)],
                           check=True, capture_output=True, timeout=60)
            subprocess.run([str(binary)], check=True, capture_output=True, timeout=10)
