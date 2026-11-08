"""Exercise the installed CLI command against the private coordinator and peer."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import signal
import sys
import threading
import time
from simulator_services import PeerServices

folder, port = Path(sys.argv[1]), sys.argv[2]
admin = ["headscale", "--config", str(folder / "config.json")]
relay_only = os.environ.get("FEMTO_CLI_RELAY_ONLY") == "1"
report = Path("output/headscale-cli.json")
report.unlink(missing_ok=True)

def run(args, success=True, input=None):
    result = subprocess.run(list(map(str, args)), capture_output=True, text=True, timeout=60, input=input)
    if success and result.returncode != 0:
        raise RuntimeError(result.stderr)
    return result

def nodes():
    return json.loads(run(admin + ["nodes", "list", "--output", "json"]).stdout) or []

def command(identity, ca, *extra):
    return ["target/debug/femto", "tailnet-connect", identity, "--host", "localhost", "--endpoint", f"127.0.0.1:{port}", "--coordinator-ca", ca, "--hostname", "femto-cli-test", *extra]

auth = ["--auth-key-file", folder / "auth.key", "--register-only"]
wrong = run(command(folder / "wrong-cli", folder / "wrong.der", *auth), success=False)
assert wrong.returncode != 0 and "UnknownIssuer" in wrong.stderr
assert nodes() == []
identity = folder / "cli-identity"
base = command(identity, folder / "root.der")
run(base + auth)
before = nodes()[0]
run(base + ["--register-only"])
assert nodes()[0]["node_key"] == before["node_key"]
run(base + auth + ["--rotate-node-key"])
renewed = nodes()[0]
assert renewed["node_key"] != before["node_key"]
for key in ["id", "machine_key", "ip_addresses"]:
    assert renewed[key] == before[key]
assert not (identity / "rotation.bin").exists()
address = next(value for value in renewed["ip_addresses"] if ":" not in value)
with PeerServices(folder, port, admin, address, https=True) as services:
    credentials = folder / "cli-credentials.json"
    credential_data = json.loads((services.books / "catalog-auth.json").read_text())
    credential_data.pop("allow_http")
    credentials.write_text(json.dumps(credential_data))
    credentials.chmod(0o600)
    runtime = base + ["--udp-bind", "0.0.0.0:0", "--run-seconds", "30", "--relay-ca", folder / "root.der", "--service-ca", folder / "root.der"]
    if relay_only:
        runtime += ["--relay-only"]
    destination = folder / "acquired.epub"
    catalog = runtime + ["--opds-url", services.origin + "/feed", "--opds-credentials", credentials]
    acquisition_started = time.monotonic()
    run(catalog + ["--opds-output", destination])
    acquisition_seconds = time.monotonic() - acquisition_started
    assert destination.read_bytes() == services.epub
    browse_directory = folder / "browser-books"
    browse_directory.mkdir()
    browser_started = time.monotonic()
    browser = run(catalog + ["--opds-browse", "--opds-download-dir", browse_directory], success=False, input="p1\nd1 café.epub\nd1 café.epub\nd1 ../escape.epub\nr\nq\n")
    browser_seconds = time.monotonic() - browser_started
    if browser.returncode != 0:
        raise RuntimeError(json.dumps({"browser_stdout":browser.stdout,"browser_stderr":browser.stderr,"counts":services.counts,"errors":services.errors}))
    assert '"title": "Tailnet fixture"' in browser.stdout
    assert browser.stderr.count("Download failed:") == 2, browser.stderr
    assert (browse_directory / "café.epub").read_bytes() == services.epub
    assert not (folder / "escape.epub").exists()
    assert list(browse_directory.iterdir()) == [browse_directory / "café.epub"]
    assert services.counts == {"feed":3,"book":2,"gets":0,"puts":0}
    assert len(services.feed_received_at) == 3
    timing = {
        "acquisition_total_seconds": acquisition_seconds,
        "acquisition_first_feed_seconds": services.feed_received_at[0] - acquisition_started,
        "browser_total_seconds": browser_seconds,
        "browser_first_feed_seconds": services.feed_received_at[1] - browser_started,
        "browser_refresh_feed_seconds": services.feed_received_at[2] - browser_started,
    }
    assert 0 <= timing["acquisition_first_feed_seconds"] <= acquisition_seconds
    assert 0 <= timing["browser_first_feed_seconds"] <= timing["browser_refresh_feed_seconds"] <= browser_seconds
    print(json.dumps({"service_timing": timing}), flush=True)
    for mode, expected in [("wrong-name", "not valid for name"), ("untrusted", "UnknownIssuer")]:
        services.tls_mode = mode
        counts = dict(services.counts)
        rejected = run(catalog, success=False)
        assert rejected.returncode != 0 and expected in rejected.stderr, rejected.stderr
        deadline = time.monotonic() + 15
        while services.tls_rejections[mode] != 1 and time.monotonic() < deadline:
            time.sleep(0.1)
        assert services.tls_rejections[mode] == 1
        assert services.counts == counts
    services.tls_mode = "valid"
    book_id = hashlib.sha256(services.epub).hexdigest()
    state, checkpoint = folder / "reader.json", folder / "checkpoint.json"
    state.write_text(json.dumps({"version": 1, "layout": {"width":480,"height":800,"margin":24,"font_size":24,"line_height":1.35},"books":{book_id:{"position":{"chapter":0,"block":0,"byte":0},"bookmarks":[],"last_opened":1}}}))
    sync = runtime + ["--progress-url", services.origin + "/progress/" + book_id, "--progress-book", destination, "--progress-state", state, "--progress-checkpoint", checkpoint, "--progress-credentials", credentials]
    run(sync)
    assert json.loads(checkpoint.read_text())["baseline"] == services.remote["record"]
    services.remote["record"]["position"]["byte"] = 1
    services.remote["record"]["recorded_at"] += 1
    services.remote["etag"] += 1
    run(sync)
    assert json.loads(state.read_text())["books"][book_id]["position"] == services.remote["record"]["position"]
    assert json.loads(checkpoint.read_text())["baseline"] == services.remote["record"]
    assert services.counts == {"feed":3,"book":2,"gets":2,"puts":1}
    local = json.loads(state.read_text())
    local["books"][book_id]["position"]["byte"] = 2
    local["books"][book_id]["last_opened"] += 1
    state.write_text(json.dumps(local))
    services.remote["record"]["position"]["byte"] = 3
    services.remote["record"]["recorded_at"] += 1
    services.remote["etag"] += 1
    state_before = state.read_bytes()
    checkpoint_before = checkpoint.read_bytes()
    remote_before = json.dumps(services.remote, sort_keys=True)
    for _ in range(2):
        conflict = run(sync, success=False)
        assert conflict.returncode != 0 and "progress sync conflict" in conflict.stderr, conflict.stderr
        assert '"status": "conflict"' in conflict.stdout
        assert state.read_bytes() == state_before
        assert checkpoint.read_bytes() == checkpoint_before
        assert json.dumps(services.remote, sort_keys=True) == remote_before
        assert services.counts["puts"] == 1
    services.remote["record"]["position"] = dict(local["books"][book_id]["position"])
    services.remote["record"]["recorded_at"] = local["books"][book_id]["last_opened"]
    services.remote["etag"] += 1
    run(sync)
    assert state.read_bytes() == state_before
    assert json.loads(checkpoint.read_text())["baseline"] == services.remote["record"]
    assert services.counts == {"feed":3,"book":2,"gets":5,"puts":1}
    entered, release = threading.Event(), threading.Event()
    services.progress_gate = (entered, release)
    checkpoint_before = checkpoint.read_bytes()
    remote_before = json.dumps(services.remote, sort_keys=True)
    process = subprocess.Popen(list(map(str, sync)), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        assert entered.wait(30), "CLI did not reach held progress response"
        assert process.poll() is None
        cancelled_at = time.monotonic()
        process.send_signal(signal.SIGINT)
        stdout, stderr = process.communicate(timeout=5)
        cancellation_seconds = time.monotonic() - cancelled_at
        assert process.returncode > 0, (process.returncode, stderr)
        assert "cancel" in stderr.lower() or "interrupt" in stderr.lower(), stderr
        assert '"status": "synchronized"' not in stdout
        assert not release.is_set()
        assert state.read_bytes() == state_before
        assert checkpoint.read_bytes() == checkpoint_before
        assert json.dumps(services.remote, sort_keys=True) == remote_before
        assert services.counts["puts"] == 1
    finally:
        release.set()
        services.progress_gate = None
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)
    run(sync)
    assert state.read_bytes() == state_before
    assert checkpoint.read_bytes() == checkpoint_before
    assert services.counts == {"feed":3,"book":2,"gets":7,"puts":1}
    assert not services.errors, services.errors
    report.write_text(json.dumps({"cli_registration":True,"coordinator_ca_rejection":True,"cli_reconnect":True,"cli_rotation":True,"cli_https_acquisition":True,"cli_interactive_details_refresh_and_download":True,"cli_interactive_destination_guards":True,"cli_certificate_rejection":True,"cli_progress_create_and_download":True,"cli_progress_conflict_preserves_both_sides":True,"cli_progress_conflict_retry":True,"cli_progress_convergence_without_write":True,"cli_progress_cancel_preserves_state":True,"cli_progress_retry_after_cancel":True,"cancellation_seconds":cancellation_seconds,"service_timing":timing,"request_counts":services.counts,"relay_only":relay_only,"peer":services.peer_version,"tls_failures":services.tls_failures,"hardware_verified":False},indent=2)+"\n")
print("CLI enrollment, renewal, HTTPS acquisition and progress services passed")
