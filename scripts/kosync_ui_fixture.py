"""Exercise the native reader menu against the official KOReader service."""
import hashlib
import json
import os
from pathlib import Path
import select
import subprocess
import tempfile
import time
import urllib.request


def check(url):
    subprocess.run(["cargo", "build", "--locked", "-p", "femto", "--features", "desktop", "--bin", "femto-simulator"], check=True)
    username, password = "native-fixture", "isolated-native-password"
    key = hashlib.md5(password.encode()).hexdigest()

    def peer(method, path, body=None):
        request = urllib.request.Request(url + path, method=method,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Accept":"application/vnd.koreader.v1+json", "Content-Type":"application/json", "x-auth-user":username, "x-auth-key":key})
        with urllib.request.urlopen(request, timeout=5) as response:
            return json.load(response)

    peer("POST", "/users/create", {"username":username,"password":key})
    book = Path("tests/fixtures/sample.epub")
    raw = book.read_bytes()
    book_id = hashlib.sha256(raw).hexdigest()
    sampled = hashlib.md5()
    for offset in [0] + [1024 << (2 * i) for i in range(11)]:
        chunk = raw[offset:offset+1024]
        if not chunk:
            break
        sampled.update(chunk)
    document = sampled.hexdigest()
    with tempfile.TemporaryDirectory(prefix="femto-kosync-ui-") as folder:
        folder = Path(folder)
        config, state = folder / "kosync.json", folder / "reader.json"
        config.write_text(json.dumps({"server":url,"username":username,"password":password,"device_id":"native-fixture","allow_http":True}))
        config.chmod(0o600)
        bookmark = {"label":"Keep bookmark","location":{"chapter":0,"block":1,"byte":3}}
        state.write_text(json.dumps({"version":1,"layout":{"width":480,"height":800,"margin":24,"font_size":24,"line_height":1.35},"books":{book_id:{"position":bookmark["location"],"bookmarks":[bookmark],"last_opened":123}}}))
        read_fd, write_fd = os.pipe()
        xserver = subprocess.Popen(["Xvfb","-noreset","-displayfd",str(write_fd),"-screen","0","800x1000x24","-nolisten","tcp"],pass_fds=(write_fd,),stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
        os.close(write_fd)
        app = None
        try:
            assert select.select([read_fd], [], [], 15)[0], "Xvfb startup timed out"
            env = dict(os.environ, DISPLAY=":" + os.read(read_fd, 32).decode().strip())
            env.pop("WAYLAND_DISPLAY", None)
            def xdo(*args):
                result = subprocess.run(["xdotool",*args],env=env,capture_output=True,text=True,timeout=5)
                return result.stdout.strip()

            def wait(check, description):
                deadline = time.monotonic() + 20
                while time.monotonic() < deadline:
                    if app.poll() is not None:
                        raise RuntimeError(f"reader exited during {description}: {app.stderr.read().decode()}")
                    value = check()
                    if value:
                        return value
                    time.sleep(0.05)
                raise RuntimeError("timeout: " + description)

            app = subprocess.Popen(["target/debug/femto-simulator",str(book),"--font","tests/fixtures/DejaVuSerif.ttf","--state",str(state),"--kosync-config",str(config)],env=env,stderr=subprocess.PIPE)
            window = wait(lambda: xdo("search","--name","^Femto"), "window")
            xdo("windowfocus", window)
            wait(lambda: " - page " in xdo("getwindowname",window), "reader startup")
            xdo("key","--delay","150","Tab",*(["Down"] * 10),"Return")
            wait(lambda: "KOReader sync" in xdo("getwindowname",window), "sync menu")
            xdo("key","--delay","150","Down","Return")
            remote = wait(lambda: peer("GET","/syncs/progress/"+document), "menu upload")
            assert remote["progress"] == "/body[1]/DocFragment[1]/body[1]/p[1]/text()[1].3"
            assert remote["device_id"] == "native-fixture"
            wait(lambda: "Reading position uploaded" in xdo("getwindowname",window), "upload completion")
            peer("PUT","/syncs/progress",{"document":document,"progress":"/body/DocFragment[1]/body/h1/text().5","percentage":0.1,"device":"Independent fixture","device_id":"peer"})
            xdo("key","--delay","150","Down","Down","Return")
            target = {"chapter":0,"block":0,"byte":5}
            def saved():
                return json.loads(state.read_text())["books"][book_id]
            wait(lambda: saved()["position"] == target, "menu download publication")
            assert saved()["bookmarks"] == [bookmark]
            xdo("key","q")
            assert app.wait(timeout=10) == 0, app.stderr.read().decode()
            assert saved()["position"] == target
        finally:
            os.close(read_fd)
            if app is not None and app.poll() is None:
                app.terminate()
                app.wait(timeout=10)
            xserver.terminate()
            xserver.wait(timeout=10)
    return {"native_menu_upload_download":True,"independent_peer_exchange":True,"bookmarks_preserved":True}
