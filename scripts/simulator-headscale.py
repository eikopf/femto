"""Drive native enrollment against the caller's isolated Headscale fixture."""
import fcntl
import json
import os
from pathlib import Path
import select
import subprocess
import sys
import time
from PIL import ImageGrab
from simulator_services import PeerServices

root = Path(__file__).resolve().parent.parent
os.chdir(root)
folder = Path(sys.argv[1])
port = int(sys.argv[2])
relay_only = os.environ.get("FEMTO_SIMULATOR_RELAY_ONLY") == "1"
https = os.environ.get("FEMTO_SIMULATOR_HTTPS") == "1"
output = root / "output/simulator-headscale"
output.mkdir(parents=True, exist_ok=True)
report = root / "output/headscale-simulator.json"
report.unlink(missing_ok=True)
admin = ["headscale", "--config", str(folder / "config.json")]


def nodes():
    result = subprocess.run(admin + ["nodes", "list", "--output", "json"], check=True, capture_output=True, text=True, timeout=5)
    value = json.loads(result.stdout)
    if value is None:
        print("Headscale returned a null empty node list", flush=True)
        return []
    if not isinstance(value, list):
        raise RuntimeError("unexpected Headscale node-list response")
    return value


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
        return subprocess.run(["xdotool", *args], env=env, check=True, capture_output=True, text=True, timeout=5)

    def wait_for(check, description):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if app.poll() is not None:
                raise RuntimeError(f"simulator exited during {description}: {app.stderr.read().decode()}")
            value = check()
            if value:
                return value
            time.sleep(0.1)
        ImageGrab.grab(xdisplay=display).save(output / "timeout.png")
        raise RuntimeError("timeout: " + description)

    def configure(name):
        directory = folder / name
        directory.mkdir(mode=0o700)
        config = directory / "tailnet.json"
        config.write_text(json.dumps({"version": 1, "host": "localhost", "port": port, "hostname": "femto-native-test", "auth_key": (folder / "auth.key").read_text().strip()}))
        config.chmod(0o600)
        return directory

    def start(directory, ca, extra=(), existing=False):
        global app
        if relay_only:
            extra = (*extra, "--tailnet-relay-only", "--tailnet-relay-ca", str(folder / "root.der"))
        if https:
            extra = (*extra, "--tailnet-service-ca", str(folder / "root.der"))
        app = subprocess.Popen(["target/debug/femto-simulator", "tests/fixtures/sample.epub", "--font", "tests/fixtures/DejaVuSerif.ttf", "--state", str(directory / "reader.json"), "--tailnet-directory", str(directory), "--tailnet-endpoint", f"127.0.0.1:{port}", "--tailnet-ca", str(ca), *extra], env=env, stderr=subprocess.PIPE)
        window = wait_for(lambda: subprocess.run(["xdotool", "search", "--name", "^Femto"], env=env, capture_output=True, text=True, timeout=5).stdout.strip(), "window")
        xdo("windowfocus", window)
        wait_for(lambda: " - page " in xdo("getwindowname", window).stdout, "reader startup")
        index = 11 if "--catalog-directory" in extra else 10
        xdo("key", "--delay", "100", "Tab", *(["Down"] * index), "Return", *([] if existing else ["Down"]), "Return")
        return window

    def unlocked(directory):
        path = directory / "identity" / "owner.lock"
        if not path.exists():
            return False
        with path.open("rb") as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                return False
            fcntl.flock(lock, fcntl.LOCK_UN)
            return True

    def quit():
        xdo("key", "q")
        assert app.wait(timeout=10) == 0, app.stderr.read().decode()

    def capture(name):
        def visible_frame():
            frame = ImageGrab.grab(xdisplay=display)
            return frame if frame.convert("L").getextrema()[1] > 0 else None
        wait_for(visible_frame, "visible native frame").save(output / name)

    wrong = configure("native-wrong-ca")
    start(wrong, folder / "wrong.der")
    wait_for(lambda: (wrong / "identity" / "identity.bin").exists() and unlocked(wrong), "untrusted registration cleanup")
    assert nodes() == [], "wrong coordinator CA registered a node"
    time.sleep(0.2)
    capture("untrusted.png")
    quit()

    directory = configure("native-tailnet")
    start(directory, folder / "root.der")

    def online_node():
        current = nodes()
        return current[0] if len(current) == 1 and current[0].get("online") is True else None

    before = wait_for(online_node, "authorized streaming node")
    assert not unlocked(directory), "connected simulator released identity lock"
    time.sleep(0.5)
    capture("connected.png")
    xdo("key", "--delay", "150", "Down", "Down", "Return")
    wait_for(lambda: unlocked(directory) and not online_node(), "disconnect and lock release")
    time.sleep(0.2)
    xdo("key", "Return")
    reconnected = wait_for(online_node, "existing identity reconnect")
    for field in ["id", "node_key", "machine_key", "ip_addresses"]:
        assert before[field] == reconnected[field], f"reconnect changed {field}"
    xdo("key", "--delay", "150", "Down", "Down", "Return")
    wait_for(lambda: unlocked(directory) and not online_node(), "disconnect before renewal")
    time.sleep(0.2)
    xdo("key", "--delay", "150", "Down", "Down", "Down", "Return")

    def renewed_node():
        node = online_node()
        return node if node and node["node_key"] != before["node_key"] else None

    renewed = wait_for(renewed_node, "authorized renewed node")
    for field in ["id", "machine_key", "ip_addresses"]:
        assert before[field] == renewed[field], f"renewal changed {field}"
    assert not (directory / "identity" / "rotation.bin").exists()
    time.sleep(0.5)
    capture("renewed.png")
    quit()
    assert unlocked(directory)
    local_address = next(address for address in renewed["ip_addresses"] if ":" not in address)
    with PeerServices(folder, port, admin, local_address, https=https) as services:
        def native_online():
            return next((node for node in nodes() if node["id"] == renewed["id"] and node.get("online") is True), None)

        start(directory, folder / "root.der", ("--catalog-directory", str(services.books)), existing=True)
        wait_for(native_online, "native catalog runtime online")
        xdo("key", "--delay", "100", "Escape", "Escape", "Tab", *(["Down"] * 10), "Return")
        wait_for(lambda: services.counts["feed"] == 1, "tailnet catalog request")
        time.sleep(0.5)
        capture("catalog.png")
        xdo("key", "--delay", "150", "Down", "Return")
        time.sleep(0.2)
        xdo("key", "--delay", "150", "Down", "Down", "Return")
        wait_for(services.acquired.exists, "tailnet EPUB publication")
        assert services.acquired.read_bytes() == services.epub
        assert not list(services.books.glob(".femto-download-*.part"))
        quit()
        assert unlocked(directory)

        if https:
            for mode in ["wrong-name", "untrusted"]:
                services.tls_mode = mode
                counts_before_rejection = dict(services.counts)
                window = start(directory, folder / "root.der", ("--catalog-directory", str(services.books)), existing=True)
                wait_for(native_online, "native certificate rejection runtime online")
                xdo("key", "--delay", "100", "Escape", "Escape", "Tab", *(["Down"] * 10), "Return")
                title = wait_for(lambda: (title if "invalid peer certificate" in (title := xdo("getwindowname", window).stdout) else None), "client certificate rejection")
                print("Client TLS rejection: " + title.strip(), flush=True)
                expected = "not valid for name" if mode == "wrong-name" else "UnknownIssuer"
                assert expected in title, title
                try:
                    wait_for(lambda: services.tls_rejections[mode] == 1 or services.errors, "native TLS rejection: " + mode)
                except RuntimeError:
                    print(json.dumps({"counts": services.counts, "tls_failures": services.tls_failures, "errors": services.errors}), flush=True)
                    raise
                assert not services.errors, services.errors
                print("Service TLS rejection: " + json.dumps(services.tls_failures[-1]), flush=True)
                time.sleep(0.3)
                capture("tls-" + mode + ".png")
                assert services.counts == counts_before_rejection, "HTTP credentials crossed rejected TLS"
                quit()
            services.tls_mode = "valid"

        def saved():
            return next(iter(json.loads((directory / "reader.json").read_text())["books"].values()))

        start(directory, folder / "root.der", ("--sync-directory", str(services.books)), existing=True)
        wait_for(native_online, "native sync runtime online")
        xdo("key", "--delay", "100", "Escape", "Escape", "Tab", *(["Down"] * 11), "Return", "Return")
        wait_for(lambda: saved().get("sync") is not None, "tailnet progress creation")
        assert services.counts["puts"] == 1
        assert saved()["sync"]["baseline"] == services.remote["record"]
        services.remote["record"]["position"] = {"chapter": 0, "block": 0, "byte": 1}
        services.remote["record"]["recorded_at"] += 1
        services.remote["etag"] += 1
        xdo("key", "Return")
        wait_for(lambda: saved()["position"] == services.remote["record"]["position"], "tailnet remote progress application")
        assert saved()["sync"]["baseline"] == services.remote["record"]
        assert services.counts["puts"] == 1
        capture("sync.png")
        quit()
        checkpoint = saved()["sync"]
        counts = dict(services.counts)
        start(directory, folder / "root.der", ("--sync-directory", str(services.books)), existing=True)
        wait_for(native_online, "native runtime after progress reopen")
        assert saved()["position"] == services.remote["record"]["position"]
        assert saved()["sync"] == checkpoint
        assert services.counts == counts, "reopen started an unsolicited service exchange"
        quit()
        assert not services.errors, services.errors
        assert services.counts == {"feed": 1, "book": 1, "gets": 2, "puts": 1}
        peer_version = services.peer_version
        tls_rejections = dict(services.tls_rejections)
        tls_failures = list(services.tls_failures)
    report.write_text(json.dumps({"native_enrollment": True, "untrusted_coordinator_rejected": True, "streaming_node_online": True, "explicit_disconnect": True, "existing_identity_reconnect": True, "node_key_renewed": True, "coordinator_node_continuity": True, "rotation_committed": True, "quit_releases_identity_lock": True, "native_tailnet_catalog": True, "native_tailnet_epub_exact": True, "native_tailnet_progress_create": True, "native_tailnet_progress_download": True, "native_tailnet_progress_reopen": True, "service_authentication": True, "service_counts": counts, "official_peer": peer_version, "relay_only": relay_only, "https": https, "tls_rejections": tls_rejections, "tls_failures": tls_failures, "coordinator": subprocess.run(["headscale", "version"], check=True, capture_output=True, text=True).stdout.strip(), "hardware_verified": False}, indent=2) + "\n")
    print("Native simulator enrollment, renewal, tailnet catalog and progress services passed", flush=True)
finally:
    if app is not None and app.poll() is None:
        app.terminate()
        app.wait(timeout=10)
    os.close(read_fd)
    server.terminate()
    server.wait(timeout=5)
