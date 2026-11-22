"""Verify remote binding controls and persistence on an isolated X server."""
import json
import os
from pathlib import Path
import select
import shutil
import subprocess
import tempfile
import time
from PIL import ImageGrab

root = Path(__file__).resolve().parent.parent
os.chdir(root)
output = root / "output/remote-editor-native"
output.mkdir(parents=True, exist_ok=True)


def wait_for(check, process, description):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"process exited during {description}: {process.returncode}")
        value = check()
        if value:
            return value
        time.sleep(0.05)
    raise RuntimeError(f"timeout: {description}")


with tempfile.TemporaryDirectory(prefix="femto-simulator-") as directory:
    state = Path(directory) / "state.json"
    library = Path(directory) / "books"
    library.mkdir()
    shutil.copyfile("tests/fixtures/sample.epub", library / "sample.epub")
    read_fd, write_fd = os.pipe()
    server = subprocess.Popen(["Xvfb", "-displayfd", str(write_fd), "-screen", "0", "800x1000x24", "-nolisten", "tcp"], pass_fds=(write_fd,), stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    os.close(write_fd)
    app = None
    try:
        if not select.select([read_fd], [], [], 15)[0]:
            raise RuntimeError("Xvfb did not report a display")
        display = ":" + os.read(read_fd, 32).decode().strip()
        env = dict(os.environ, DISPLAY=display)
        env.pop("WAYLAND_DISPLAY", None)

        def xdo(*args):
            return subprocess.run(["xdotool", *args], env=env, capture_output=True, text=True, timeout=5)

        def saved():
            if not state.exists():
                return None
            data = json.loads(state.read_text())
            return next(iter(data["books"].values()), None)

        def start(book="tests/fixtures/sample.epub", wait_ready=True, extra=()):
            process = subprocess.Popen(["target/debug/femto-simulator", book, "--font", "tests/fixtures/DejaVuSerif.ttf", "--fallback-font", "tests/fixtures/DejaVuSans.ttf", "--state", str(state), "--library", str(library), *extra], env=env, stderr=subprocess.PIPE)
            window = wait_for(lambda: xdo("search", "--name", "^Femto").stdout.strip(), process, "native window")
            assert xdo("windowfocus", window).returncode == 0
            if wait_ready:
                wait_for(lambda: " - page " in xdo("getwindowname", window).stdout, process, "startup indexing")
            return process, window

        app, window = start()
        assert xdo("key", "b").returncode == 0
        wait_for(lambda: state.exists(), app, "initial saved bookmark")
        baseline = json.loads(state.read_text())

        def volume_menu():
            assert xdo("key", "--delay", "150", "Tab", *(["Down"] * 7), "Return", "Return", *(["Down"] * 6)).returncode == 0

        def binding(action):
            entries = json.loads(state.read_text()).get("remote_bindings") or []
            return any(entry == {"usage": 0xc00e9, "action": action} for entry in entries)

        def frame():
            geometry = dict(line.split("=", 1) for line in xdo("getwindowgeometry", "--shell", window).stdout.splitlines())
            x, y = int(geometry["X"]), int(geometry["Y"])
            return ImageGrab.grab(xdisplay=display).crop((x, y, x + 480, y + 800))

        volume_menu()
        assert xdo("key", "Return").returncode == 0
        wait_for(lambda: binding("previous"), app, "volume opt-in")
        time.sleep(0.3)
        first = frame()
        first.save(output / "volume-previous.png")
        assert xdo("key", "--delay", "150", "BackSpace", "BackSpace", "BackSpace", "q").returncode == 0
        assert app.wait(timeout=10) == 0, app.stderr.read().decode()
        app, window = start()
        assert binding("previous")
        volume_menu()
        wait_for(lambda: frame().tobytes() == first.tobytes(), app, "identical reopened menu")
        frame().save(output / "volume-reopened.png")
        assert xdo("key", "Return").returncode == 0
        wait_for(lambda: binding("next"), app, "volume next")
        assert xdo("key", "Return").returncode == 0
        wait_for(lambda: not binding("next") and not binding("previous"), app, "volume off")
        assert xdo("key", "--delay", "150", "BackSpace", "BackSpace", "BackSpace").returncode == 0
        volume_menu()
        assert xdo("key", "--delay", "150", *(["Up"] * 5), "Return").returncode == 0
        wait_for(lambda: json.loads(state.read_text()).get("remote_bindings") == [], app, "disable buttons")
        assert xdo("key", "Return").returncode == 0
        wait_for(lambda: json.loads(state.read_text()).get("remote_bindings") is None, app, "restore defaults")
        final = json.loads(state.read_text())
        for book_id, book in baseline["books"].items():
            for key in ("position", "bookmarks", "clippings"):
                assert final["books"][book_id].get(key) == book.get(key)
        assert final["layout"] == baseline["layout"]
        (output / "result.json").write_text(json.dumps({
            "status": "passed", "volume_opt_in_cycles_previous_next_off": True,
            "mapping_persisted_after_quit_reopen": True,
            "reopened_menu_pixels_identical": True,
            "disable_and_defaults_work": True,
            "position_bookmarks_clippings_and_layout_unchanged": True,
            "physical_device_test": False,
        }, indent=2) + "\n")
        assert xdo("key", "q").returncode == 0
        assert app.wait(timeout=10) == 0, app.stderr.read().decode()
        print("Native remote binding menu passed")
    finally:
        if app is not None and app.poll() is None:
            app.terminate()
            app.wait(timeout=10)
        server.terminate()
        server.wait(timeout=10)
        os.close(read_fd)
