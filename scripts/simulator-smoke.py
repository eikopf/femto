"""Exercise the native window on an isolated X server; no desktop interaction."""
import json
import http.server
import socketserver
import fcntl
import threading
import hashlib
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
output = root / "output/simulator-smoke"
output.mkdir(parents=True, exist_ok=True)


def wait_for(check, process, description):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"process exited during {description}: {process.returncode}: {process.stderr.read().decode()}")
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
    server = subprocess.Popen(["Xvfb", "-noreset", "-displayfd", str(write_fd), "-screen", "0", "800x1000x24", "-nolisten", "tcp"], pass_fds=(write_fd,), stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
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
        assert xdo("key", "Right").returncode == 0
        first = wait_for(lambda: saved() and saved()["position"]["block"] > 0 and saved(), app, "page turn")
        assert xdo("key", "b").returncode == 0
        wait_for(lambda: saved() and len(saved()["bookmarks"]) == 1, app, "bookmark")
        ImageGrab.grab(xdisplay=display).save(output / "reading.png")
        assert xdo("key", "--delay", "150", "Tab", "Down", "Down", "Return").returncode == 0
        time.sleep(0.2)
        ImageGrab.grab(xdisplay=display).save(output / "chapters.png")
        assert xdo("key", "--delay", "150", "Down", "Return").returncode == 0
        wait_for(lambda: saved() and saved()["position"]["chapter"] == 1, app, "chapter menu selection")
        assert xdo("key", "BackSpace").returncode == 0
        wait_for(lambda: saved() and saved()["position"] == first["position"], app, "back navigation")
        assert xdo("key", "--delay", "150", "Tab", *(["Down"] * 10), "Return", "Return").returncode == 0
        wait_for(lambda: len(saved().get("clippings", [])) == 1, app, "native page clipping")
        clipping = saved()["clippings"][0]
        assert clipping["text"]
        assert tuple(clipping["range"]["start"][key] for key in ("chapter", "block", "byte")) < tuple(
            clipping["range"]["end"][key] for key in ("chapter", "block", "byte")
        )
        before_capture = state.stat().st_mtime_ns
        assert xdo("key", "Return").returncode == 0
        wait_for(lambda: state.stat().st_mtime_ns != before_capture, app, "repeat page clipping")
        assert saved()["clippings"] == [clipping]
        assert saved()["position"] == first["position"]
        assert xdo("key", "--delay", "150", "BackSpace", "BackSpace").returncode == 0
        assert xdo("key", "q").returncode == 0
        assert app.wait(timeout=10) == 0, app.stderr.read().decode()
        saved_before_cancel = state.read_bytes()
        slow_book = Path(directory) / "cancel-startup.epub"
        slow_book.write_bytes(b"x" * (2 * 1024 * 1024))
        app, window = start(str(slow_book), wait_ready=False)
        assert "Opening book" in xdo("getwindowname", window).stdout
        assert xdo("key", "Escape").returncode == 0
        assert app.wait(timeout=10) == 0, app.stderr.read().decode()
        assert state.read_bytes() == saved_before_cancel
        cache = library / ".femto-library-cache.json"
        cached_before_skip = cache.read_bytes()
        slow_scan = library / "slow-scan.epub"
        slow_scan.write_bytes(b"x" * (2 * 1024 * 1024))
        app, window = start(wait_ready=False)
        wait_for(lambda: "Scanning library" in xdo("getwindowname", window).stdout, app, "startup library scan")
        assert xdo("key", "Escape").returncode == 0
        wait_for(lambda: " - page " in xdo("getwindowname", window).stdout, app, "skipped startup scan")
        assert state.read_bytes() == saved_before_cancel
        assert cache.read_bytes() == cached_before_skip
        assert xdo("key", "q").returncode == 0
        assert app.wait(timeout=10) == 0, app.stderr.read().decode()
        slow_scan.unlink()
        app, window = start()
        first_id = next(iter(json.loads(state.read_text())["books"]))
        assert saved()["position"] == first["position"]
        assert len(saved()["bookmarks"]) == 1
        assert saved()["clippings"] == [clipping]
        assert xdo("key", "Right").returncode == 0
        away = wait_for(lambda: saved()["position"] != first["position"] and saved()["position"], app, "move away from clipping")
        assert xdo("key", "--delay", "150", "Tab", *(["Down"] * 10), "Return", "Down", "Return", "Down", "Return").returncode == 0
        wait_for(lambda: saved()["position"] == clipping["range"]["start"], app, "clipping source navigation")
        assert xdo("key", "BackSpace").returncode == 0
        wait_for(lambda: saved()["position"] == away, app, "clipping return history")
        assert xdo("key", "Left").returncode == 0
        wait_for(lambda: saved()["position"] == first["position"], app, "restore page after clipping jump")
        assert xdo("key", "--delay", "150", "Tab", *(["Down"] * 10), "Return", "Down", "Return", "Return").returncode == 0
        time.sleep(0.2)
        ImageGrab.grab(xdisplay=display).save(output / "clipping-delete.png")
        before_keep = state.stat().st_mtime_ns
        assert xdo("key", "Return").returncode == 0  # Keep is selected by default.
        wait_for(lambda: state.stat().st_mtime_ns != before_keep, app, "keep clipping")
        assert saved()["clippings"] == [clipping]
        assert xdo("key", "--delay", "150", "Down", "Return", "Return", "Down", "Return").returncode == 0
        wait_for(lambda: not saved().get("clippings"), app, "confirmed native clipping deletion")
        assert saved()["position"] == first["position"]
        assert len(saved()["bookmarks"]) == 1
        assert xdo("key", "--delay", "150", "BackSpace", "BackSpace").returncode == 0

        def click(x, y):
            before = state.stat().st_mtime_ns
            assert xdo("mousemove", "--window", window, str(x), str(y)).returncode == 0
            assert xdo("mousedown", "1").returncode == 0
            try:
                wait_for(lambda: state.stat().st_mtime_ns != before, app, "pointer action")
            finally:
                assert xdo("mouseup", "1").returncode == 0
            time.sleep(0.1)

        assert xdo("key", "--delay", "150", "Tab", *(["Down"] * 10), "Return", "Down", "Return").returncode == 0
        time.sleep(0.2)
        selection_frame = ImageGrab.grab(xdisplay=display)
        selection_frame.save(output / "word-selection.png")
        geometry = dict(line.split("=", 1) for line in xdo("getwindowgeometry", "--shell", window).stdout.splitlines())
        sx, sy = int(geometry["X"]), int(geometry["Y"])
        sw, sh = int(geometry["WIDTH"]), int(geometry["HEIGHT"])
        overlap = selection_frame.crop((sx, sy + 32, sx + sw, sy + sh - 32)).tobytes()
        assert xdo("key", "Down").returncode == 0

        def selection_pan_frame():
            frame = ImageGrab.grab(xdisplay=display)
            actual = frame.crop((sx, sy, sx + sw, sy + sh - 64)).tobytes()
            return frame if actual == overlap else None

        wait_for(selection_pan_frame, app, "native selection pixel pan").save(output / "word-selection-panned.png")
        assert saved()["position"] == first["position"]
        assert xdo("key", "Up").returncode == 0
        original_body = selection_frame.crop((sx, sy, sx + sw, sy + sh - 32)).tobytes()
        wait_for(lambda: ImageGrab.grab(xdisplay=display).crop((sx, sy, sx + sw, sy + sh - 32)).tobytes() == original_body,
            app, "restore selection pixel view")
        click(40, 40)  # First source word on the restored fixture page.
        assert not saved().get("clippings")
        assert saved()["position"] == first["position"]
        ImageGrab.grab(xdisplay=display).save(output / "word-preview.png")
        before_cancel = state.stat().st_mtime_ns
        assert xdo("key", "Return").returncode == 0  # Cancel is initially selected.
        wait_for(lambda: state.stat().st_mtime_ns != before_cancel, app, "cancel selected word")
        assert not saved().get("clippings")
        assert xdo("key", "--delay", "150", "Down", "Return").returncode == 0
        click(40, 40)
        assert xdo("key", "--delay", "150", "Down", "Return").returncode == 0
        wait_for(lambda: len(saved().get("clippings", [])) == 1, app, "save selected word")
        word_clipping = saved()["clippings"][0]
        assert word_clipping["text"] == clipping["text"].split()[0]
        assert word_clipping["range"]["start"] == clipping["range"]["start"]
        expected_end = dict(word_clipping["range"]["start"])
        expected_end["byte"] += len(word_clipping["text"].encode())
        assert word_clipping["range"]["end"] == expected_end
        assert saved()["position"] == first["position"]
        assert len(saved()["bookmarks"]) == 1
        assert xdo("key", "--delay", "150", "BackSpace", "BackSpace", "q").returncode == 0
        assert app.wait(timeout=10) == 0, app.stderr.read().decode()
        app, window = start()
        assert saved()["clippings"] == [word_clipping]
        assert saved()["position"] == first["position"]
        (output / "word-selection.json").write_text(json.dumps({
            "clipping": word_clipping, "position_after_reopen": saved()["position"],
            "bookmarks_after_reopen": saved()["bookmarks"],
        }, indent=2) + "\n")

        assert xdo("key", "--delay", "150", "Tab", *(["Down"] * 10), "Return", "Down", "Down", "Return").returncode == 0
        click(40, 40)
        assert xdo("key", "--delay", "150", "Down", "Down", "Return").returncode == 0  # Extend.
        geometry = dict(line.split("=", 1) for line in xdo("getwindowgeometry", "--shell", window).stdout.splitlines())
        selection_x, selection_y = int(geometry["X"]), int(geometry["Y"])

        def selected_word_frame():
            frame = ImageGrab.grab(xdisplay=display)
            region = frame.crop((selection_x + 30, selection_y + 30, selection_x + 75, selection_y + 48)).convert("L")
            return frame if sum(region.tobytes()) < 128 * region.width * region.height else None

        wait_for(selected_word_frame, app, "inverted pending word").save(output / "range-highlight.png")
        click(160, 40)  # The third source word, "font", in the fixed fixture layout.
        assert saved()["clippings"] == [word_clipping]
        assert saved()["position"] == first["position"]
        ImageGrab.grab(xdisplay=display).save(output / "range-preview.png")
        assert xdo("key", "--delay", "150", "Down", "Return").returncode == 0
        wait_for(lambda: len(saved().get("clippings", [])) == 2, app, "save extended native selection")
        range_clipping = saved()["clippings"][1]
        assert range_clipping["text"] == "when the font"
        assert clipping["text"].startswith(range_clipping["text"])
        assert range_clipping["range"]["start"] == word_clipping["range"]["start"]
        range_end = dict(range_clipping["range"]["start"])
        range_end["byte"] += len(range_clipping["text"].encode())
        assert range_clipping["range"]["end"] == range_end
        assert saved()["position"] == first["position"]
        assert len(saved()["bookmarks"]) == 1
        assert xdo("key", "--delay", "150", "BackSpace", "BackSpace", "q").returncode == 0
        assert app.wait(timeout=10) == 0, app.stderr.read().decode()
        app, window = start()
        assert saved()["clippings"] == [word_clipping, range_clipping]
        assert saved()["position"] == first["position"]
        (output / "range-selection.json").write_text(json.dumps({
            "clippings_after_reopen": saved()["clippings"],
            "position_after_reopen": saved()["position"],
            "bookmarks_after_reopen": saved()["bookmarks"],
        }, indent=2) + "\n")

        def reading_frame():
            bounds = dict(line.split("=", 1) for line in xdo("getwindowgeometry", "--shell", window).stdout.splitlines())
            left, top = int(bounds["X"]), int(bounds["Y"])
            return ImageGrab.grab(xdisplay=display).crop((left, top, left + int(bounds["WIDTH"]), top + int(bounds["HEIGHT"])))

        time.sleep(0.2)
        plain_reading = reading_frame().tobytes()

        def toggle_first_highlight():
            assert xdo("key", "--delay", "150", "Tab", *(["Down"] * 10), "Return", "Down", "Return", "Down", "Down", "Return", "BackSpace", "BackSpace", "BackSpace").returncode == 0

        toggle_first_highlight()
        wait_for(lambda: saved()["clippings"][0].get("highlighted") is True, app, "enable saved highlight")
        enabled_clipping = dict(word_clipping, highlighted=True)
        assert saved()["clippings"] == [enabled_clipping, range_clipping]
        assert saved()["position"] == first["position"]
        assert len(saved()["bookmarks"]) == 1
        wait_for(selected_word_frame, app, "visible saved highlight").save(output / "saved-highlight.png")
        highlighted_reading = reading_frame().tobytes()
        assert highlighted_reading != plain_reading
        assert xdo("key", "q").returncode == 0
        assert app.wait(timeout=10) == 0, app.stderr.read().decode()
        app, window = start()
        assert saved()["clippings"] == [enabled_clipping, range_clipping]
        wait_for(lambda: reading_frame().tobytes() == highlighted_reading, app, "highlight pixels after reopen")
        assert saved()["position"] == first["position"]
        toggle_first_highlight()
        wait_for(lambda: saved()["clippings"] == [word_clipping, range_clipping], app, "disable saved highlight")
        wait_for(lambda: reading_frame().tobytes() == plain_reading, app, "restore unhighlighted pixels")
        assert xdo("key", "q").returncode == 0
        assert app.wait(timeout=10) == 0, app.stderr.read().decode()
        app, window = start()
        assert saved()["clippings"] == [word_clipping, range_clipping]
        assert saved()["position"] == first["position"]
        assert len(saved()["bookmarks"]) == 1
        wait_for(lambda: reading_frame().tobytes() == plain_reading, app, "disabled highlight after reopen")
        (output / "saved-highlight.json").write_text(json.dumps({
            "enabled_clippings_after_reopen": [enabled_clipping, range_clipping],
            "disabled_clippings_after_reopen": saved()["clippings"],
            "position_after_reopen": saved()["position"],
            "bookmarks_after_reopen": saved()["bookmarks"],
            "enabled_and_disabled_pixels_identical_after_reopen": True,
        }, indent=2) + "\n")

        def button_word_list():
            assert xdo("key", "--delay", "150", "Tab", *(["Down"] * 10), "Return", *(["Down"] * 4), "Return").returncode == 0

        button_word_list()
        time.sleep(0.2)
        reading_frame().save(output / "button-word-list.png")
        assert xdo("key", "--delay", "150", "Down", "Return", "Return").returncode == 0  # Pick "the", then cancel.
        assert saved()["clippings"] == [word_clipping, range_clipping]
        assert xdo("key", "--delay", "150", "BackSpace", "BackSpace").returncode == 0
        wait_for(lambda: reading_frame().tobytes() == plain_reading, app, "button selection cancel pixels")
        button_word_list()
        assert xdo("key", "--delay", "150", "Down", "Return", "Down", "Down", "Return", "Down", "Down", "Return").returncode == 0
        assert saved()["clippings"] == [word_clipping, range_clipping]
        time.sleep(0.2)
        reading_frame().save(output / "button-word-preview.png")
        assert xdo("key", "--delay", "150", "Down", "Return").returncode == 0
        wait_for(lambda: len(saved().get("clippings", [])) == 3, app, "save button selected range")
        button_clipping = saved()["clippings"][2]
        assert button_clipping["text"] == "the font"
        button_start = dict(word_clipping["range"]["start"])
        button_start["byte"] += len("when ".encode())
        assert button_clipping["range"] == {"start": button_start, "end": range_end}
        assert saved()["position"] == first["position"]
        assert len(saved()["bookmarks"]) == 1
        assert xdo("key", "--delay", "150", "BackSpace", "BackSpace").returncode == 0
        wait_for(lambda: reading_frame().tobytes() == plain_reading, app, "button capture leaves reading pixels unchanged")
        assert xdo("key", "q").returncode == 0
        assert app.wait(timeout=10) == 0, app.stderr.read().decode()
        app, window = start()
        assert saved()["clippings"] == [word_clipping, range_clipping, button_clipping]
        assert saved()["position"] == first["position"]
        assert len(saved()["bookmarks"]) == 1
        wait_for(lambda: reading_frame().tobytes() == plain_reading, app, "button selection pixels after reopen")
        (output / "button-selection.json").write_text(json.dumps({
            "clippings_after_reopen": saved()["clippings"],
            "position_after_reopen": saved()["position"],
            "bookmarks_after_reopen": saved()["bookmarks"],
            "reading_pixels_unchanged_after_cancel_save_and_reopen": True,
        }, indent=2) + "\n")

        assert xdo("key", "q").returncode == 0
        assert app.wait(timeout=10) == 0, app.stderr.read().decode()
        app, window = start(extra=("--dictionary", "tests/fixtures/dictionary/example.ifo"))
        before_dictionary = saved()["clippings"]
        assert xdo("key", "--delay", "150", "Tab", *(["Down"] * 10), "Return", *(["Down"] * 5), "Return", "Down", "Down", "Return").returncode == 0
        time.sleep(0.2)
        dictionary_preview = reading_frame().tobytes()
        assert xdo("key", "--delay", "150", "Down", "Down", "Down", "Return").returncode == 0
        wait_for(lambda: "Dictionary - Backspace" in xdo("getwindowname", window).stdout, app, "native dictionary result")
        reading_frame().save(output / "dictionary-result.png")
        dictionary_result_pixels = reading_frame().tobytes()
        assert reading_frame().tobytes() != dictionary_preview
        assert saved()["clippings"] == before_dictionary
        assert saved()["position"] == first["position"]
        assert xdo("key", "BackSpace").returncode == 0
        wait_for(lambda: reading_frame().tobytes() == dictionary_preview, app, "dictionary returns to selection")
        assert xdo("key", "--delay", "150", "Down", "Down", "Down", "Return").returncode == 0
        wait_for(lambda: "Dictionary - Backspace" in xdo("getwindowname", window).stdout, app, "repeat dictionary result")
        assert reading_frame().tobytes() == dictionary_result_pixels
        assert saved()["clippings"] == before_dictionary
        assert xdo("key", "BackSpace").returncode == 0
        wait_for(lambda: reading_frame().tobytes() == dictionary_preview, app, "repeat lookup returns to selection")
        assert xdo("key", "--delay", "150", "Return", "BackSpace", "BackSpace").returncode == 0
        wait_for(lambda: reading_frame().tobytes() == plain_reading, app, "dictionary leaves book pixels unchanged")
        assert saved()["clippings"] == before_dictionary
        assert len(saved()["bookmarks"]) == 1
        (output / "dictionary-result.json").write_text(json.dumps({
            "clippings_unchanged": True, "position": saved()["position"],
            "bookmarks": saved()["bookmarks"], "selection_and_reading_pixels_restored": True, "repeat_lookup_pixels_identical": True,
        }, indent=2) + "\n")

        dictionaries = Path(directory) / "dictionaries"
        dictionaries.mkdir()
        for name in ("a", "b"):
            for suffix in ("ifo", "idx", "dict", "syn"):
                source = Path(f"tests/fixtures/dictionary/example.{suffix}")
                destination = dictionaries / f"{name}.{suffix}"
                shutil.copyfile(source, destination)
                if suffix == "ifo":
                    lines = destination.read_text().splitlines()
                    destination.write_text("\n".join(
                        f"bookname=Dictionary {name.upper()}" if line.startswith("bookname=") else line
                        for line in lines
                    ) + "\n")
        assert xdo("key", "q").returncode == 0
        assert app.wait(timeout=10) == 0, app.stderr.read().decode()
        app, window = start(extra=("--dictionary-directory", str(dictionaries)))
        assert xdo("key", "--delay", "150", "Tab", *(["Down"] * 11), "Return", "Down", "Return").returncode == 0
        wait_for(lambda: json.loads(state.read_text()).get("dictionary_name") == "b.ifo", app, "persist dictionary choice")
        reading_frame().save(output / "dictionary-chooser.png")
        assert xdo("key", "--delay", "150", "BackSpace", "BackSpace").returncode == 0
        wait_for(lambda: reading_frame().tobytes() == plain_reading, app, "chooser leaves reading pixels unchanged")
        chosen_result = None
        for reopened in (False, True):
            if reopened:
                assert xdo("key", "q").returncode == 0
                assert app.wait(timeout=10) == 0, app.stderr.read().decode()
                app, window = start(extra=("--dictionary-directory", str(dictionaries)))
            assert json.loads(state.read_text())["dictionary_name"] == "b.ifo"
            assert xdo("key", "--delay", "150", "Tab", *(["Down"] * 10), "Return", *(["Down"] * 5), "Return", "Down", "Down", "Return", "Down", "Down", "Down", "Return").returncode == 0
            wait_for(lambda: "Dictionary - Backspace" in xdo("getwindowname", window).stdout, app, "chosen dictionary result")
            result = reading_frame().tobytes()
            assert result != dictionary_result_pixels
            if reopened:
                assert result == chosen_result
                reading_frame().save(output / "dictionary-choice-reopened.png")
            else:
                chosen_result = result
            assert saved()["clippings"] == before_dictionary
            assert saved()["position"] == first["position"]
            assert len(saved()["bookmarks"]) == 1
            assert xdo("key", "--delay", "150", "BackSpace", "Return", "BackSpace", "BackSpace").returncode == 0
            wait_for(lambda: reading_frame().tobytes() == plain_reading, app, "chosen dictionary returns to reading")
        (output / "dictionary-choice.json").write_text(json.dumps({
            "dictionary_name_after_reopen": json.loads(state.read_text())["dictionary_name"],
            "chosen_result_pixels_identical_after_reopen": True,
            "reading_pixels_clippings_position_bookmarks_unchanged": True,
        }, indent=2) + "\n")

        click(240, 400)  # Reader menu.
        click(400, 750)  # Second menu page.
        click(240, 150)  # Typography.
        click(240, 150)  # Larger text.
        wait_for(lambda: json.loads(state.read_text())["layout"]["font_size"] == 26, app, "pointer typography")
        geometry = dict(line.split("=", 1) for line in xdo("getwindowgeometry", "--shell", window).stdout.splitlines())
        window_x, window_y = int(geometry["X"]), int(geometry["Y"])

        def typography_frame():
            frame = ImageGrab.grab(xdisplay=display)
            # The second control row has text only after the reflow panel closes.
            row = frame.crop((window_x + 20, window_y + 220, window_x + 440, window_y + 280))
            return frame if row.convert("L").getextrema()[0] < 128 else None

        wait_for(typography_frame, app, "completed typography frame").save(output / "typography.png")
        click(240, 250)  # Restore the original size.
        wait_for(lambda: json.loads(state.read_text())["layout"]["font_size"] == 24, app, "restore typography")
        after_reflow = saved()["position"]
        assert after_reflow == first["position"]
        click(400, 750)  # Second typography page.
        click(240, 150)  # Disable automatic dictionary hyphenation.
        wait_for(lambda: json.loads(state.read_text())["layout"]["hyphenation"] == "off", app, "hyphenation setting")
        assert saved()["position"] == after_reflow
        click(400, 750)  # Reflow returns to the first typography page.
        click(240, 350)  # Increase line spacing.
        wait_for(lambda: json.loads(state.read_text())["layout"]["line_height"] == 1.6, app, "line spacing")
        assert saved()["typography"]["line_height"] == 1.6
        assert saved()["position"] == after_reflow
        click(400, 750)
        click(240, 450)  # Increase margins.
        wait_for(lambda: json.loads(state.read_text())["layout"]["margin"] == 32, app, "page margins")
        assert saved()["typography"]["margin"] == 32
        assert saved()["position"] == after_reflow
        click(400, 750)
        click(240, 250)  # Reset this book to default typography.
        wait_for(lambda: json.loads(state.read_text())["layout"]["hyphenation"] == "book", app, "reset book typography")
        assert saved()["typography"]["hyphenation"] == "book"
        assert saved()["position"] == after_reflow
        click(240, 50)  # Back to root.
        click(400, 750)  # Second menu page.
        click(240, 350)  # Library.
        shutil.copyfile("tests/fixtures/sample.txt", library / "second.txt")
        click(240, 550)  # Rescan picks up a book added after startup.
        wait_for(lambda: "Scanning library" not in xdo("getwindowname", window).stdout, app, "library rescan")
        click(240, 150)  # All books.
        ImageGrab.grab(xdisplay=display).save(output / "library.png")
        click(240, 50)  # Back to library.
        click(240, 450)  # Search.
        wait_for(lambda: "Library search" in xdo("getwindowname", window).stdout, app, "search editor")
        assert xdo("type", "--delay", "100", "qsecond.txz").returncode == 0
        assert app.poll() is None  # Typing q must not quit the editor.
        assert xdo("key", "BackSpace").returncode == 0
        assert xdo("type", "t").returncode == 0
        ImageGrab.grab(xdisplay=display).save(output / "search-keyboard.png")
        assert xdo("key", "Return").returncode == 0
        wait_for(lambda: "Library search" not in xdo("getwindowname", window).stdout, app, "empty search results")
        ImageGrab.grab(xdisplay=display).save(output / "search-empty.png")
        click(240, 50)  # Return to the query.
        wait_for(lambda: "Library search" in xdo("getwindowname", window).stdout, app, "restored query editor")
        time.sleep(0.1)
        click(300, 750)  # Clear.
        assert xdo("type", "--delay", "100", "second.txt").returncode == 0
        assert xdo("key", "Return").returncode == 0
        wait_for(lambda: "Library search" not in xdo("getwindowname", window).stdout, app, "search results")
        ImageGrab.grab(xdisplay=display).save(output / "search-results.png")
        click(240, 150)  # Open the matching book.
        wait_for(lambda: len(json.loads(state.read_text())["books"]) == 2, app, "pointer book selection")
        ImageGrab.grab(xdisplay=display).save(output / "second-book.png")
        assert xdo("key", "q").returncode == 0
        assert app.wait(timeout=10) == 0, app.stderr.read().decode()
        original = json.loads(state.read_text())["books"][first_id]
        assert original["position"] == after_reflow
        assert len(original["bookmarks"]) == 1
        state = Path(directory) / "links-state.json"
        app, window = start("tests/fixtures/navigation.epub")
        assert xdo("key", "b").returncode == 0
        wait_for(saved, app, "link fixture state")
        time.sleep(0.2)
        ImageGrab.grab(xdisplay=display).save(output / "links.png")
        origin = saved()["position"]
        click(140, 90)  # Underlined footnote reference in the default layout.
        wait_for(lambda: saved()["position"]["chapter"] == 1, app, "direct link tap")
        assert xdo("key", "BackSpace").returncode == 0
        wait_for(lambda: saved()["position"] == origin, app, "link tap back history")
        assert xdo("key", "q").returncode == 0
        assert app.wait(timeout=10) == 0, app.stderr.read().decode()
        state = Path(directory) / "notes-state.json"
        app, window = start("tests/fixtures/noterefs.epub")
        assert xdo("key", "b").returncode == 0
        wait_for(saved, app, "note fixture state")
        origin = saved()["position"]
        bookmarks = saved()["bookmarks"]

        def preview_ready():
            return "Link preview 1/1" in xdo("getwindowname", window).stdout

        click(70, 40)  # EPUB-marked note reference on the first line.
        wait_for(preview_ready, app, "cooperative note preview")
        assert saved()["position"] == origin
        assert saved()["bookmarks"] == bookmarks
        ImageGrab.grab(xdisplay=display).save(output / "note-preview.png")
        click(20, 750)  # Close directly to the source reading page.
        wait_for(lambda: "preview" not in xdo("getwindowname", window).stdout, app, "preview close")
        assert saved()["position"] == origin
        assert xdo("key", "BackSpace").returncode == 0
        time.sleep(0.2)
        assert saved()["position"] == origin
        click(70, 40)
        wait_for(preview_ready, app, "reopened note preview")
        click(400, 750)  # Follow the note outside the reading spine.
        wait_for(lambda: saved()["position"]["chapter"] > 0, app, "explicit note follow")
        assert xdo("key", "BackSpace").returncode == 0
        wait_for(lambda: saved()["position"] == origin, app, "preview follow back history")
        assert saved()["bookmarks"] == bookmarks
        assert xdo("key", "q").returncode == 0
        assert app.wait(timeout=10) == 0, app.stderr.read().decode()
        epub = (root / "tests/fixtures/sample.epub").read_bytes()
        requests = []
        class CatalogHandler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def do_GET(self):
                if self.headers.get("Authorization") != "Basic cmVhZGVyOnNlY3JldA==":
                    self.send_error(401)
                    return
                requests.append(self.path)
                if self.path == "/feed":
                    body = json.dumps({"metadata": {"title": "Simulator catalog"},
                        "links": [{"rel": "self", "href": "/feed", "type": "application/opds+json"}],
                        "publications": [{"metadata": {"title": "Download fixture"},
                            "links": [{"rel": "http://opds-spec.org/acquisition/open-access",
                                "href": "/book.epub", "type": "application/epub+zip"}]}]}).encode()
                    media = "application/opds+json"
                elif self.path == "/book.epub":
                    body, media = epub, "application/epub+zip"
                else:
                    self.send_error(404)
                    return
                self.send_response(200)
                self.send_header("Content-Type", media)
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

        catalog_server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), CatalogHandler)
        catalog_thread = threading.Thread(target=catalog_server.serve_forever, daemon=True)
        catalog_thread.start()
        try:
            (library / "catalog.url").write_text(f"http://127.0.0.1:{catalog_server.server_port}/feed")
            (library / "catalog-auth.json").write_text(json.dumps({"username": "reader", "password": "secret", "allow_http": True}))
            state = Path(directory) / "catalog-state.json"
            app, window = start(extra=("--catalog-directory", str(library)))
            assert xdo("key", "--delay", "100", "Tab", *(["Down"] * 10), "Return").returncode == 0
            wait_for(lambda: "/feed" in requests, app, "authenticated catalog feed")
            time.sleep(0.5)
            ImageGrab.grab(xdisplay=display).save(output / "catalog.png")
            assert xdo("key", "--delay", "150", "Down", "Return").returncode == 0
            time.sleep(0.2)
            assert xdo("key", "--delay", "150", "Down", "Down", "Return").returncode == 0
            downloaded = library / ("book-" + hashlib.sha256(epub).hexdigest() + ".epub")
            wait_for(downloaded.exists, app, "catalog EPUB publication")
            assert downloaded.read_bytes() == epub
            assert requests == ["/feed", "/book.epub"]
            assert not list(library.glob(".femto-download-*.part"))
            assert xdo("key", "q").returncode == 0
            assert app.wait(timeout=10) == 0, app.stderr.read().decode()
        finally:
            catalog_server.shutdown()
            catalog_server.server_close()
            catalog_thread.join(timeout=5)
        # Hold TLS setup on loopback to exercise enrollment cancellation without credentials.
        tailnet_root = Path(directory) / "tailnet"
        tailnet_root.mkdir(mode=0o700)
        release_tls = threading.Event()
        accepted_tls = []

        class StalledTLS(socketserver.BaseRequestHandler):
            def handle(self):
                accepted_tls.append(self.client_address)
                release_tls.wait(15)

        class TailnetServer(socketserver.ThreadingTCPServer):
            daemon_threads = True

        tailnet_server = TailnetServer(("127.0.0.1", 0), StalledTLS)
        tailnet_thread = threading.Thread(target=tailnet_server.serve_forever, daemon=True)
        tailnet_thread.start()
        port = tailnet_server.server_address[1]
        config = tailnet_root / "tailnet.json"
        config.write_text(json.dumps({"version": 1, "host": "control.example", "port": port, "hostname": "femto-test"}))
        config.chmod(0o600)
        try:
            app, window = start(extra=("--tailnet-directory", str(tailnet_root), "--tailnet-endpoint", f"127.0.0.1:{port}"))
            assert xdo("key", "--delay", "100", "Tab", *(["Down"] * 10), "Return", "Return").returncode == 0
            time.sleep(0.4)
            assert not (tailnet_root / "identity").exists(), "Connect must not create an identity"
            assert xdo("key", "--delay", "150", "Down", "Return").returncode == 0
            wait_for(lambda: len(accepted_tls) == 1, app, "simulator enrollment TLS setup")
            identity = tailnet_root / "identity" / "identity.bin"
            identity_bytes = identity.read_bytes()
            ImageGrab.grab(xdisplay=display).save(output / "tailnet-connecting.png")
            assert xdo("key", "--delay", "150", "Down", "Down", "Return").returncode == 0

            def identity_unlocked():
                with (tailnet_root / "identity" / "owner.lock").open("rb") as lock:
                    try:
                        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    except BlockingIOError:
                        return False
                    fcntl.flock(lock, fcntl.LOCK_UN)
                    return True

            wait_for(identity_unlocked, app, "cancelled enrollment releases identity lock")
            time.sleep(0.2)
            assert identity.read_bytes() == identity_bytes
            assert xdo("key", "--delay", "150", "Down", "Return").returncode == 0
            time.sleep(0.4)
            assert identity.read_bytes() == identity_bytes, "Provision must not replace an existing identity"
            assert len(accepted_tls) == 1
            assert xdo("key", "Return").returncode == 0
            wait_for(lambda: len(accepted_tls) == 2, app, "existing identity reconnect after cancellation")
            assert xdo("key", "q").returncode == 0
            assert app.wait(timeout=5) == 0, app.stderr.read().decode()
            assert not release_tls.is_set(), "quit must cancel setup before the server responds"
            assert identity_unlocked()
            assert identity.read_bytes() == identity_bytes
        finally:
            release_tls.set()
            tailnet_server.shutdown()
            tailnet_server.server_close()
            tailnet_thread.join(timeout=5)
        print("Native X11 reader, catalog, plus tailnet setup cancellation, identity preservation, reconnect and quit passed")
    finally:
        os.close(read_fd)
        if app is not None and app.poll() is None:
            app.terminate()
            app.wait(timeout=5)
        server.terminate()
        server.wait(timeout=5)
