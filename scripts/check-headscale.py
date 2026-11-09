"""Independent coordinator probe using temporary loopback-only Headscale state."""
import json
from datetime import datetime, timezone
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time
import selectors
import ssl
import base64
import secrets
import sys
from http.server import BaseHTTPRequestHandler, HTTPServer

ROOT = Path(__file__).resolve().parents[1]
os.chdir(ROOT)
OUTPUT = ROOT / "output/headscale-probe.json"
MIGRATION_ONLY = os.environ.get("FEMTO_HEADSCALE_MIGRATION_ONLY") == "1"
CLI_ONLY = os.environ.get("FEMTO_HEADSCALE_CLI_ONLY") == "1"
RUNTIME_ONLY = os.environ.get("FEMTO_HEADSCALE_RUNTIME_ONLY") == "1"
SIMULATOR_ONLY = os.environ.get("FEMTO_HEADSCALE_SIMULATOR_ONLY") == "1"
RUNTIME_OUTPUT = ROOT / "output/headscale-runtime.json"
if MIGRATION_ONLY:
    (ROOT / "output/headscale-migration.json").unlink(missing_ok=True)
(ROOT / "output/headscale-cli.json" if CLI_ONLY else ROOT / "output/headscale-simulator.json" if SIMULATOR_ONLY else RUNTIME_OUTPUT if RUNTIME_ONLY else OUTPUT).unlink(missing_ok=True)


def run(command, **kwargs):
    return subprocess.run(command, check=True, capture_output=True, text=True, timeout=60, **kwargs)


def read_until(process, marker, received=b""):
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        deadline = time.monotonic() + 30
        while marker not in received:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not selector.select(remaining):
                raise RuntimeError("map probe readiness timed out")
            chunk = os.read(process.stdout.fileno(), 4096)
            if not chunk:
                _, stderr = process.communicate(timeout=5)
                raise RuntimeError("map probe closed before readiness: " + stderr.decode().strip())
            received += chunk
    return received


def independent_tcp_peer(folder, coordinator_port, auth, command, local_address, peer_address, relay=False):
    payload = bytes(index % 251 for index in range(65536))
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        listener.settimeout(0.1)
        policy = folder / "tcp-policy.json"
        policy.write_text(json.dumps({"acls": [{"action": "accept", "src": [local_address],
            "dst": [f"{peer_address}:{listener.getsockname()[1]}"], "proto": "tcp"}]}))
        run(command + ["policy", "set", "--file", str(policy)])
        probe = subprocess.Popen([
            "target/debug/examples/headscale-probe", f"127.0.0.1:{coordinator_port}",
            str(folder / "root.der"), str(auth), str(folder / "identity"),
            "derp-tcp-peer" if relay else "tcp-peer", str(listener.getsockname()[1]),
        ], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        received = 0
        try:
            deadline = time.monotonic() + 100
            connection = None
            while probe.poll() is None and time.monotonic() < deadline:
                try:
                    connection, source = listener.accept()
                    break
                except socket.timeout:
                    continue
            if connection is None:
                if probe.poll() is not None:
                    _, stderr = probe.communicate(timeout=5)
                    raise RuntimeError("independent TCP probe exited before connection: " + stderr.decode().strip())
                probe.kill()
                stdout, stderr = probe.communicate(timeout=5)
                raise RuntimeError("independent TCP peer did not connect: " + stdout.decode().strip() + " " + stderr.decode().strip())
            with connection:
                if source[0] != "127.0.0.1":
                    raise RuntimeError("independent TCP peer used an unexpected source")
                connection.settimeout(45 if MIGRATION_ONLY else 20)
                received = 0
                while received < len(payload):
                    chunk = connection.recv(997)
                    if not chunk or chunk != payload[received:received + len(chunk)]:
                        raise RuntimeError("independent TCP peer payload mismatch or premature EOF")
                    received += len(chunk)
                    connection.sendall(chunk)
                connection.shutdown(socket.SHUT_WR)
                if connection.recv(1) != b"":
                    raise RuntimeError("independent TCP peer did not close after echo")
            stdout, stderr = probe.communicate(timeout=15)
            print(stdout.decode(), end="", flush=True)
            if probe.returncode:
                raise RuntimeError("independent TCP probe failed: " + stderr.decode().strip())
            result = json.loads(next(line for line in stdout.decode().splitlines() if line.startswith("{")))
            if not all(result[field] is True for field in ["independent_peer_tcp", "remote_eof", "tcp_closed"]):
                raise RuntimeError("independent TCP lifecycle evidence missing")
            if relay and (not result["derp_traffic"] or result["direct_udp_sent"] != 0):
                raise RuntimeError("relay-only TCP evidence missing")
            if result["renewal_elapsed_ms"] < 195000 or result["authenticated_session_installations"] < 2:
                raise RuntimeError("independent TCP renewal evidence missing")
            if (result["deliberately_dropped_initiations"] != 1
                    or result["timer_only_handshake_recovery"] is not True
                    or result["deliberately_dropped_responses"] != 1
                    or result["handshake_retry_recovery_ms"] < 10000):
                raise RuntimeError("independent timer-only handshake recovery evidence missing")
            if result["bytes_written"] != len(payload) or result["bytes_read"] != len(payload):
                raise RuntimeError("independent TCP transfer evidence missing")
            if (result["deliberately_dropped_data_packets"] != 3
                    or result["data_drop_ciphertext_bytes"] <= 256
                    or result["data_drop_installed_sessions"] < 2
                    or not 49152 <= result["data_drop_written_bytes"] <= len(payload)
                    or result["data_drop_read_bytes"] >= len(payload)
                    or result["data_drop_at_ms"] <= result["response_drop_at_ms"]):
                raise RuntimeError("independent transport-data loss evidence missing")
            drops = result["data_drop_events"]
            if (len(drops) != 3 or any(
                    drop["at_ms"] < result["data_drop_at_ms"]
                    or drop["ciphertext_bytes"] <= 256
                    or drop["installed_sessions"] < 2
                    or not 49152 <= drop["written_bytes"] <= len(payload)
                    or drop["read_bytes"] >= len(payload)
                    for drop in drops)):
                raise RuntimeError("independent three-packet loss evidence missing")
            if [drop["at_ms"] for drop in drops] != sorted(drop["at_ms"] for drop in drops):
                raise RuntimeError("independent packet-loss timing is not ordered")
            if MIGRATION_ONLY:
                migration = result["path_migration"]
                if (migration["direct_data_sent"][0] == 0 or migration["direct_data_sent"][2] == 0
                        or migration["direct_data_sent"][1] != 0
                        or migration["relay_packets_queued"][1] == 0
                        or migration["relay_events"][1] == 0
                        or migration["outage_send_errors"] == 0
                        or not migration["read_at_outage"] < migration["read_at_recovery"] < result["bytes_read"]):
                    raise RuntimeError("independent direct/DERP/direct migration evidence missing")
            return result
        except Exception as error:
            if probe.poll() is None:
                probe.kill()
            stdout, stderr = probe.communicate(timeout=5)
            raise RuntimeError(f"independent TCP stage failed after {received}/{len(payload)} received bytes: {error}; probe stdout: {stdout.decode().strip()}; probe stderr: {stderr.decode().strip()}") from error
        finally:
            if probe.poll() is None:
                probe.kill()
            probe.communicate(timeout=5)


def independent_tls_peer(folder, coordinator_port, auth, command, local_address, peer_address, relay=False):
    def path(name):
        return str(folder / name)
    run(["openssl", "req", "-new", "-newkey", "rsa:2048", "-nodes",
         "-subj", "/CN=femto-independent-peer.femto.test",
         "-addext", "subjectAltName=DNS:femto-independent-peer.femto.test",
         "-keyout", path("peer.key"), "-out", path("peer.csr")])
    for name, ca in [("peer", "root"), ("untrusted-peer", "wrong")]:
        run(["openssl", "x509", "-req", "-in", path("peer.csr"), "-CA", path(ca + ".pem"),
             "-CAkey", path(ca + ".key"), "-CAcreateserial", "-days", "1", "-copy_extensions", "copy",
             "-out", path(name + ".pem")])
    for name, certificate, private_key, rejection in [
        ("valid", "peer.pem", "peer.key", None),
        ("wrong-name", "server.pem", "server.key", "NotValidForName"),
        ("untrusted", "untrusted-peer.pem", "peer.key", "UnknownIssuer"),
        ("truncated", "peer.pem", "peer.key", "UnexpectedEof"),
    ]:
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(path(certificate), path(private_key))
        context.set_alpn_protocols(["http/1.1"])
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            listener.listen(1)
            listener.settimeout(0.1)
            policy = folder / "tls-policy.json"
            policy.write_text(json.dumps({"acls": [{"action": "accept", "src": [local_address],
                "dst": [f"{peer_address}:{listener.getsockname()[1]}"], "proto": "tcp"}]}))
            run(command + ["policy", "set", "--file", str(policy)])
            probe = subprocess.Popen([
                "target/debug/examples/headscale-probe", f"127.0.0.1:{coordinator_port}",
                path("root.der"), str(auth), path("identity"), "derp-tls-peer" if relay else "tls-peer", str(listener.getsockname()[1]),
            ], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                deadline = time.monotonic() + 40
                connection = None
                while probe.poll() is None and time.monotonic() < deadline:
                    try:
                        connection, source = listener.accept()
                        break
                    except socket.timeout:
                        continue
                if connection is None:
                    raise RuntimeError("independent TLS peer did not connect")
                with connection:
                    connection.settimeout(10)
                    if source[0] != "127.0.0.1":
                        raise RuntimeError("independent TLS peer used an unexpected source")
                    try:
                        stream = context.wrap_socket(connection, server_side=True)
                    except (ssl.SSLError, TimeoutError, ConnectionResetError):
                        if name in ["valid", "truncated"]:
                            raise
                    else:
                        with stream:
                            if name in ["wrong-name", "untrusted"]:
                                raise RuntimeError("untrusted or wrong-name TLS certificate was accepted")
                            if stream.selected_alpn_protocol() != "http/1.1":
                                raise RuntimeError("TLS HTTP ALPN was not negotiated")
                            request = b""
                            while not request.endswith(b"\r\n\r\n") and len(request) < 4096:
                                chunk = stream.recv(1024)
                                if not chunk:
                                    raise RuntimeError("TLS peer closed before HTTP request")
                                request += chunk
                            if request != b"GET /fixture HTTP/1.1\r\nHost: femto-independent-peer.femto.test\r\nConnection: close\r\n\r\n":
                                raise RuntimeError("TLS peer HTTP request mismatch")
                            body = bytes(index % 251 for index in range(65536))
                            stream.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 65536\r\nConnection: close\r\n\r\n" + body)
                            if name == "truncated":
                                os.close(stream.detach())
                            else:
                                try:
                                    stream.unwrap()
                                except (ssl.SSLError, ConnectionResetError, TimeoutError):
                                    # unwrap sends close_notify first. The client verifies it,
                                    # then drops its managed flow without a blocking TLS close.
                                    pass
                stdout, stderr = probe.communicate(timeout=15)
                if rejection is not None:
                    if probe.returncode == 0 or rejection not in stderr.decode():
                        raise RuntimeError(f"TLS {name} rejection evidence missing: {stderr.decode().strip()}")
                    print(f"independent {'DERP ' if relay else ''}TLS {name} rejected", flush=True)
                else:
                    print(stdout.decode(), end="", flush=True)
                    if probe.returncode:
                        raise RuntimeError("independent TLS probe failed: " + stderr.decode().strip())
                    result = json.loads(next(line for line in stdout.decode().splitlines() if line.startswith("{")))
                    if not all(result[field] is True for field in ["independent_peer_tls", "tls_verified", "tls_close_notify"]):
                        raise RuntimeError("independent TLS exchange evidence missing")
                    if relay and (not result["derp_traffic"] or result["direct_udp_sent"] != 0 or result["relay_events"] == 0):
                        raise RuntimeError("independent TLS relay-only evidence missing")
                    if result["http_body_bytes"] != 65536:
                        raise RuntimeError("independent TLS HTTP body evidence missing")
            finally:
                if probe.poll() is None:
                    probe.kill()
                probe.communicate(timeout=5)


def independent_catalog_peer(folder, coordinator_port, command, local_address, peer_address, relay=False):
    password = secrets.token_hex(24)
    credentials = folder / "catalog-auth.json"
    credentials.write_text(json.dumps({"username": "fixture", "password": password}))
    credentials.chmod(0o600)
    expected_auth = "Basic " + base64.b64encode(f"fixture:{password}".encode()).decode()
    book = ROOT / "tests/fixtures/book-api.epub"
    book_bytes = book.read_bytes()
    counts = {"denied": 0, "redirect": 0, "feed": 0, "book": 0}
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"
        def log_message(self, *_):
            pass
        def do_GET(self):
            self.close_connection = True
            if self.headers.get("Authorization") != expected_auth:
                counts["denied"] += 1
                self.send_response(401)
                self.send_header("WWW-Authenticate", 'Basic realm="fixture"')
                self.send_header("Content-Length", "0")
                self.end_headers()
            elif self.path == "/start":
                counts["redirect"] += 1
                self.send_response(302)
                self.send_header("Location", "/catalog/feed.json")
                self.send_header("Content-Length", "0")
                self.end_headers()
            elif self.path == "/catalog/feed.json":
                counts["feed"] += 1
                feed = json.dumps({"metadata": {"title": "Independent fixture"},
                    "links": [{"rel": "self", "href": "/catalog/feed.json", "type": "application/opds+json"}], "publications": [
                    {"metadata": {"title": "Fixture book", "identifier": "urn:femto:fixture"}, "links": [
                        {"href": "../books/book.epub", "type": "application/epub+zip",
                         "rel": "http://opds-spec.org/acquisition/open-access"}]}]}).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/opds+json")
                self.send_header("Transfer-Encoding", "chunked")
                self.end_headers()
                for offset in range(0, len(feed), 113):
                    chunk = feed[offset:offset + 113]
                    self.wfile.write(f"{len(chunk):x}\r\n".encode() + chunk + b"\r\n")
                self.wfile.write(b"0\r\nX-Fixture: complete\r\n\r\n")
            elif self.path == "/books/book.epub":
                counts["book"] += 1
                self.send_response(200)
                self.send_header("Content-Type", "application/epub+zip")
                self.send_header("Content-Length", str(len(book_bytes)))
                self.end_headers()
                self.wfile.write(book_bytes)
            else:
                self.send_error(404)
    with HTTPServer(("127.0.0.1", 0), Handler) as server:
        server.timeout = 0.1
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(str(folder / "peer.pem"), str(folder / "peer.key"))
        context.set_alpn_protocols(["http/1.1"])
        server.socket = context.wrap_socket(server.socket, server_side=True)
        server.socket.settimeout(10)
        port = server.server_address[1]
        policy = folder / "catalog-policy.json"
        policy.write_text(json.dumps({"acls": [{"action": "accept", "src": [local_address],
            "dst": [f"{peer_address}:{port}"], "proto": "tcp"}]}))
        run(command + ["policy", "set", "--file", str(policy)])
        output = folder / ("relay-acquired.epub" if relay else "acquired.epub")
        probe = subprocess.Popen([
            "target/debug/examples/headscale-services", f"127.0.0.1:{coordinator_port}",
            str(folder / "root.der"), str(folder / "identity"),
            f"https://femto-independent-peer.femto.test:{port}/start", str(credentials),
            str(output), str(book),
        ] + (["relay"] if relay else []), stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            deadline = time.monotonic() + 75
            while probe.poll() is None:
                if time.monotonic() >= deadline:
                    raise RuntimeError("independent catalog probe timed out")
                server.handle_request()
            stdout, stderr = probe.communicate(timeout=5)
            print(stdout.decode(), end="", flush=True)
            if probe.returncode:
                raise RuntimeError("independent catalog probe failed: " + stderr.decode().strip())
            if counts != {"denied": 1, "redirect": 1, "feed": 1, "book": 1} or output.read_bytes() != book_bytes:
                raise RuntimeError("independent catalog server or acquisition evidence missing")
            result = json.loads(next(line for line in stdout.decode().splitlines() if line.startswith("{")))
            if result["joined_shutdown"] is not True:
                raise RuntimeError("catalog control worker shutdown evidence missing")
            if relay and (not result["derp_traffic"] or result["direct_udp_sent"] != 0 or result["relay_events"] == 0):
                raise RuntimeError("catalog relay-only evidence missing")
            for field in ["independent_opds", "unauthenticated_denied", "authenticated_redirect", "relative_acquisition", "epub_validated", "epub_exact", "epub_noclobber"]:
                if result[field] is not True:
                    raise RuntimeError("independent catalog evidence missing: " + field)
        finally:
            if probe.poll() is None:
                probe.kill()
            probe.communicate(timeout=5)


def independent_peer(folder, coordinator_port, auth, command, local_address):
    """Run an official peer with private state and no kernel networking."""
    policy = folder / "peer-policy.json"
    policy.write_text(json.dumps({"acls": [{"action": "accept", "src": ["*"], "dst": ["*:*"]}]}))
    run(command + ["policy", "set", "--file", str(policy)])
    peer_socket = str(folder / "tailscaled.sock")
    environment = dict(os.environ, SSL_CERT_FILE=str(folder / "root.pem"),
                       TS_DISABLE_PORTMAPPER="true", TS_NO_LOGS_NO_SUPPORT="true")
    cli = ["tailscale", "--socket", peer_socket]
    with open(folder / "tailscaled.log", "w+") as log:
        daemon = subprocess.Popen([
            "tailscaled", "--tun=userspace-networking", "--socket=" + peer_socket,
            "--state=" + str(folder / "tailscale.state"), "--statedir=" + str(folder / "tailscale"),
            "--port=0", "--no-logs-no-support",
        ], stdout=log, stderr=log, env=environment)
        try:
            deadline = time.monotonic() + 10
            while not Path(peer_socket).exists():
                if daemon.poll() is not None or time.monotonic() >= deadline:
                    raise RuntimeError("private userspace Tailscale startup failed")
                time.sleep(0.05)
            run(cli + ["up", f"--login-server=https://localhost:{coordinator_port}",
                       "--auth-key=file:" + str(auth), "--hostname=femto-independent-peer",
                       "--accept-dns=false", "--accept-routes=false", "--netfilter-mode=off", "--timeout=20s"],
                env=environment)
            status = json.loads(run(cli + ["status", "--json"], env=environment).stdout)
            print("independent peer DNS name: " + status["Self"]["DNSName"], flush=True)
            if MIGRATION_ONLY:
                peer_address = next(address for address in status["Self"]["TailscaleIPs"] if ":" not in address)
                result = independent_tcp_peer(folder, coordinator_port, auth, command, local_address, peer_address)
                return run(cli + ["version"]).stdout.splitlines()[0], {"migration": result}
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as echo:
                echo.bind(("127.0.0.1", 0))
                echo.settimeout(0.1)
                relay_probe = subprocess.Popen([
                    "target/debug/examples/headscale-probe", f"127.0.0.1:{coordinator_port}",
                    str(folder / "root.der"), str(auth), str(folder / "identity"),
                    "derp-peer", str(echo.getsockname()[1]),
                ], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                count = 0
                try:
                    deadline = time.monotonic() + 45
                    while relay_probe.poll() is None:
                        if time.monotonic() >= deadline:
                            raise RuntimeError("independent DERP probe timed out")
                        try:
                            payload, source = echo.recvfrom(2048)
                        except socket.timeout:
                            continue
                        if payload != b"femto independent Tailscale UDP probe" or source[0] != "127.0.0.1":
                            raise RuntimeError("DERP peer delivered unexpected echo input")
                        echo.sendto(payload, source)
                        count += 1
                    stdout, stderr = relay_probe.communicate(timeout=5)
                    print(stdout.decode(), end="", flush=True)
                    if relay_probe.returncode:
                        raise RuntimeError("independent DERP probe failed: " + stderr.decode().strip())
                    result = json.loads(next(line for line in stdout.decode().splitlines() if line.startswith("{")))
                    if count != 1 or not result["derp_traffic"] or result["direct_udp_sent"] != 0 or not result["tls_verified"]:
                        raise RuntimeError("independent DERP evidence missing")
                finally:
                    if relay_probe.poll() is None:
                        relay_probe.kill()
                    relay_probe.communicate(timeout=5)
            with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as echo:
                echo.bind(("127.0.0.1", 0))
                echo.settimeout(0.1)
                probe = subprocess.Popen([
                    "target/debug/examples/headscale-probe", f"127.0.0.1:{coordinator_port}",
                    str(folder / "root.der"), str(auth), str(folder / "identity"),
                    "udp-peer", str(echo.getsockname()[1]),
                ], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                count = 0
                try:
                    deadline = time.monotonic() + 45
                    while probe.poll() is None:
                        if time.monotonic() >= deadline:
                            raise RuntimeError("independent peer UDP probe timed out")
                        try:
                            payload, source = echo.recvfrom(2048)
                        except socket.timeout:
                            continue
                        if payload != b"femto independent Tailscale UDP probe" or source[0] != "127.0.0.1":
                            raise RuntimeError("independent peer delivered unexpected echo input")
                        echo.sendto(payload, source)
                        count += 1
                    stdout, stderr = probe.communicate(timeout=5)
                    print(stdout.decode(), end="", flush=True)
                    if probe.returncode:
                        raise RuntimeError("independent peer probe failed: " + stderr.decode().strip())
                    result = json.loads(next(line for line in stdout.decode().splitlines() if line.startswith("{")))
                    if count != 1 or not result["independent_peer_udp"] or not result["wireguard_peer_traffic"]:
                        raise RuntimeError("independent peer UDP exchange evidence missing")
                    peer_address = next(address for address in status["Self"]["TailscaleIPs"] if ":" not in address)
                    independent_tls_peer(folder, coordinator_port, auth, command, local_address, peer_address, relay=True)
                    independent_catalog_peer(folder, coordinator_port, command, local_address, peer_address, relay=True)
                    relay_tcp = independent_tcp_peer(folder, coordinator_port, auth, command, local_address, peer_address, relay=True)
                    direct_tcp = independent_tcp_peer(folder, coordinator_port, auth, command, local_address, peer_address)
                    independent_tls_peer(folder, coordinator_port, auth, command, local_address, peer_address)
                    independent_catalog_peer(folder, coordinator_port, command, local_address, peer_address)
                    return run(cli + ["version"]).stdout.splitlines()[0], {"relay": relay_tcp, "direct": direct_tcp}
                finally:
                    if probe.poll() is None:
                        probe.kill()
                    probe.communicate(timeout=5)
        finally:
            daemon.terminate()
            try:
                daemon.wait(timeout=5)
            except subprocess.TimeoutExpired:
                daemon.kill()
                daemon.wait(timeout=5)


with tempfile.TemporaryDirectory(prefix="femto-headscale-") as directory:
    folder = Path(directory)
    def path(name):
        return str(folder / name)

    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
         "-subj", "/CN=Femto local test root", "-addext", "basicConstraints=critical,CA:TRUE",
         "-keyout", path("root.key"), "-out", path("root.pem")])
    run(["openssl", "req", "-new", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=localhost",
         "-addext", "subjectAltName=DNS:localhost", "-keyout", path("server.key"), "-out", path("server.csr")])
    run(["openssl", "x509", "-req", "-in", path("server.csr"), "-CA", path("root.pem"),
         "-CAkey", path("root.key"), "-CAcreateserial", "-days", "1", "-copy_extensions", "copy", "-out", path("server.pem")])
    run(["openssl", "x509", "-in", path("root.pem"), "-outform", "DER", "-out", path("root.der")])
    run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
         "-subj", "/CN=Untrusted fixture root", "-keyout", path("wrong.key"), "-out", path("wrong.pem")])
    run(["openssl", "x509", "-in", path("wrong.pem"), "-outform", "DER", "-out", path("wrong.der")])
    (folder / "derp.yaml").write_text(f"""regions:
  999:
    regionid: 999
    regioncode: fixture
    regionname: Local fixture
    nodes:
      - name: fixture
        regionid: 999
        hostname: localhost
        ipv4: ""
        ipv6: none
        stunport: -1
        derpport: {port}
""")
    config = {
        "server_url": f"https://localhost:{port}", "listen_addr": f"127.0.0.1:{port}",
        "metrics_listen_addr": "", "grpc_listen_addr": "127.0.0.1:0",
        "noise": {"private_key_path": path("noise.key")},
        "prefixes": {"v4": "100.64.0.0/24", "v6": "fd7a:115c:a1e0::/48"},
        "database": {"type": "sqlite", "sqlite": {"path": path("db.sqlite")}},
        "policy": {"mode": "database"},
        "derp": {"urls": [], "paths": [path("derp.yaml")], "auto_update_enabled": False,
                 "server": {"enabled": True, "region_id": 999, "region_code": "fixture",
                            "region_name": "Local fixture", "verify_clients": True,
                            "stun_listen_addr": "127.0.0.1:0", "private_key_path": path("derp.key"),
                            "automatically_add_embedded_derp_region": False}},
        "disable_check_updates": True, "tls_cert_path": path("server.pem"), "tls_key_path": path("server.key"),
        "dns": {"magic_dns": False, "base_domain": "femto.test", "override_local_dns": False, "nameservers": {"global": []}},
        "unix_socket": path("headscale.sock"), "unix_socket_permission": "0700",
        "log": {"level": "warn"},
    }
    (folder / "config.json").write_text(json.dumps(config))
    command = ["headscale", "--config", path("config.json")]
    run(command + ["configtest"])
    with open(path("server.log"), "w+") as log:
        server = subprocess.Popen(command + ["serve"], stdout=log, stderr=log)
        try:
            until = time.monotonic() + 15
            while True:
                if server.poll() is not None:
                    log.seek(0)
                    raise RuntimeError("Headscale startup failed: " + log.read()[-2000:])
                if Path(path("headscale.sock")).exists():
                    break
                if time.monotonic() >= until:
                    raise RuntimeError("Headscale startup timed out")
                time.sleep(0.05)
            user = json.loads(run(command + ["users", "create", "femto", "--output", "json"]).stdout)
            key = json.loads(run(command + ["preauthkeys", "create", "--user", str(user["id"]), "--reusable", "--output", "json"]).stdout)
            auth = folder / "auth.key"
            auth.write_text(key["key"])
            auth.chmod(0o600)
            if MIGRATION_ONLY:
                registration = run(["target/debug/examples/headscale-probe", f"127.0.0.1:{port}",
                                    path("root.der"), str(auth), path("identity"), "register"])
                record = next(json.loads(line) for line in registration.stdout.splitlines()
                              if line.startswith("{") and "assigned_ipv4" in json.loads(line))
                peer_version, measurements = independent_peer(folder, port, auth, command, record["assigned_ipv4"])
                output = ROOT / "output/headscale-migration.json"
                output.parent.mkdir(exist_ok=True)
                output.write_text(json.dumps({"status": "passed", "hardware_verified": False,
                    "headscale": run(["headscale", "version"]).stdout.splitlines()[0],
                    "tailscale_peer": peer_version, "measurements": measurements}, indent=2) + "\n")
                print("Independent TCP direct/DERP/direct migration passed", flush=True)
                sys.exit(0)
            if CLI_ONLY:
                subprocess.run([sys.executable, "scripts/cli-headscale.py", str(folder), str(port)], check=True, timeout=180)
                sys.exit(0)
            if SIMULATOR_ONLY:
                subprocess.run([sys.executable, "scripts/simulator-headscale.py", str(folder), str(port)], check=True, timeout=180)
                sys.exit(0)
            if RUNTIME_ONLY:
                arguments = ["target/debug/examples/host-runtime-probe", f"127.0.0.1:{port}"]
                wrong = subprocess.run(arguments + [path("wrong.der"), str(auth), path("wrong-runtime-identity")], capture_output=True, text=True, timeout=30)
                if wrong.returncode == 0 or "UnknownIssuer" not in wrong.stderr:
                    raise RuntimeError("application runtime accepted an untrusted coordinator")
                if json.loads(run(command + ["nodes", "list", "--output", "json"]).stdout):
                    raise RuntimeError("untrusted application runtime registered a node")
                probe = subprocess.Popen(arguments + [path("root.der"), str(auth), path("runtime-identity")], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                try:
                    received = read_until(probe, b"application runtime map ready\n")
                    before = json.loads(run(command + ["nodes", "list", "--output", "json"]).stdout)
                    server.kill()
                    server.wait(timeout=5)
                    received = read_until(probe, b"application runtime map revoked\n", received)
                    Path(path("headscale.sock")).unlink(missing_ok=True)
                    server = subprocess.Popen(command + ["serve"], stdout=log, stderr=log)
                    stdout, stderr = probe.communicate(timeout=90)
                    received += stdout
                    print(received.decode(), end="", flush=True)
                    if probe.returncode:
                        raise RuntimeError("application runtime probe failed: " + stderr.decode().strip())
                    result = json.loads(next(line for line in received.decode().splitlines() if line.startswith("{")))
                    for field in ["application_registration", "application_map", "application_reconnect", "revoked_during_restart", "closed_runtime_denies_access", "identity_lock_released"]:
                        if result.get(field) is not True:
                            raise RuntimeError("application runtime evidence missing: " + field)
                    after = json.loads(run(command + ["nodes", "list", "--output", "json"]).stdout)
                    if len(before) != 1 or len(after) != 1:
                        raise RuntimeError("application runtime node count changed")
                    for field in ["id", "node_key", "machine_key", "ip_addresses"]:
                        if before[0][field] != after[0][field]:
                            raise RuntimeError("application runtime reconnect changed " + field)
                    result.update({"coordinator": run(["headscale", "version"]).stdout.strip(), "untrusted_coordinator_rejected": True, "coordinator_node_continuity": True, "hardware_verified": False})
                    RUNTIME_OUTPUT.parent.mkdir(exist_ok=True)
                    RUNTIME_OUTPUT.write_text(json.dumps(result, indent=2) + "\n")
                    print("Independent application runtime registration and reconnect passed", flush=True)
                finally:
                    if probe.poll() is None:
                        probe.kill()
                    probe.communicate(timeout=5)
                sys.exit(0)
            untrusted = subprocess.run(["target/debug/examples/headscale-probe", f"127.0.0.1:{port}", path("wrong.der"), str(auth), path("wrong-identity"), "register"], capture_output=True, text=True, timeout=30)
            if untrusted.returncode == 0 or "UnknownIssuer" not in untrusted.stderr or (folder / "wrong-identity").exists():
                raise RuntimeError("untrusted coordinator certificate was not rejected before identity creation")
            print("untrusted coordinator certificate rejected", flush=True)
            evidence = []
            for mode in ["register", "rotate"]:
                if mode == "rotate":
                    watcher = subprocess.Popen(["target/debug/examples/headscale-probe", f"127.0.0.1:{port}", path("root.der"), str(auth), path("identity"), "watch-expiry"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
                    try:
                        received = read_until(watcher, b"map ready for expiry\n")
                        run(command + ["nodes", "expire", "--identifier", str(evidence[0][1]["id"])])
                        stdout, stderr = watcher.communicate(timeout=30)
                        print((received + stdout).decode(), end="", flush=True)
                        if watcher.returncode:
                            raise RuntimeError(f"active expiry probe failed: {stderr.decode().strip()}")
                        active = json.loads(next(line for line in (received + stdout).decode().splitlines() if line.startswith("{")))
                        if not active["authenticated_expiry"] or not active["map_authorization_revoked"]:
                            raise RuntimeError("active map expiry evidence missing")
                    finally:
                        if watcher.poll() is None:
                            watcher.kill()
                        watcher.communicate(timeout=5)
                    saved_identity = (folder / "identity/identity.bin").read_bytes()
                    expired = subprocess.run(["target/debug/examples/headscale-probe", f"127.0.0.1:{port}", path("root.der"), str(auth), path("identity"), "expect-expired"], capture_output=True, text=True, timeout=90)
                    print(expired.stdout, end="", flush=True)
                    if expired.returncode:
                        raise RuntimeError(f"expired-key probe failed: {expired.stderr.strip()}")
                    if (folder / "identity/identity.bin").read_bytes() != saved_identity or (folder / "identity/rotation.bin").exists():
                        raise RuntimeError("expired-key rejection changed the durable identity")
                probe = subprocess.run(["target/debug/examples/headscale-probe", f"127.0.0.1:{port}", path("root.der"), str(auth), path("identity"), mode], capture_output=True, text=True, timeout=90)
                print(probe.stdout, end="", flush=True)
                if probe.returncode:
                    raise RuntimeError(f"{mode} probe failed: {probe.stderr.strip()}")
                result = json.loads(next(line for line in probe.stdout.splitlines() if line.startswith("{")))
                nodes = json.loads(run(command + ["nodes", "list", "--output", "json"]).stdout)
                if len(nodes) != 1:
                    raise RuntimeError("expected exactly one coordinator node")
                evidence.append((result, nodes[0]))
            before, after = evidence
            if before[1]["id"] != after[1]["id"] or before[0]["assigned_ipv4"] != after[0]["assigned_ipv4"]:
                raise RuntimeError("rotation changed node identity or address")
            if before[0]["node_key"] == after[0]["node_key"]:
                raise RuntimeError("rotation did not change the node key")
            if before[1]["machine_key"] != after[1]["machine_key"] or before[1]["ip_addresses"] != after[1]["ip_addresses"]:
                raise RuntimeError("rotation changed coordinator machine identity or assigned addresses")
            for result, node in evidence:
                if node["node_key"].removeprefix("nodekey:") != result["node_key"].removeprefix("nodekey:"):
                    raise RuntimeError("coordinator and client disagree on the active node key")
            reconnect = subprocess.Popen(["target/debug/examples/headscale-probe", f"127.0.0.1:{port}", path("root.der"), str(auth), path("identity"), "reconnect"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                received = read_until(reconnect, b"map ready for reconnect\n")
                server.kill()
                server.wait(timeout=5)
                received = read_until(reconnect, b"control disconnected before reconnect\n", received)
                Path(path("headscale.sock")).unlink(missing_ok=True)
                server = subprocess.Popen(command + ["serve"], stdout=log, stderr=log)
                stdout, stderr = reconnect.communicate(timeout=45)
                received += stdout
                if reconnect.returncode:
                    raise RuntimeError(f"reconnect probe failed: {stderr.decode().strip()}")
                print(received.decode(), end="", flush=True)
                reconnect_result = json.loads(next(line for line in received.decode().splitlines() if line.startswith("{")))
                for field in ["coordinator_reconnect", "fresh_registration", "fresh_map_authorization", "disconnected_access_denied", "identity_record_retained", "endpoint_publication_before_restart", "endpoint_publication_after_restart"]:
                    if reconnect_result.get(field) is not True:
                        raise RuntimeError(f"reconnect did not verify {field}")
                nodes = json.loads(run(command + ["nodes", "list", "--output", "json"]).stdout)
                if len(nodes) != 1 or nodes[0]["id"] != after[1]["id"] or nodes[0]["node_key"] != after[1]["node_key"] or nodes[0]["machine_key"] != after[1]["machine_key"] or nodes[0]["ip_addresses"] != after[1]["ip_addresses"]:
                    raise RuntimeError("reconnect changed coordinator node identity or addresses")
                if reconnect_result["assigned_ipv4"] != after[0]["assigned_ipv4"]:
                    raise RuntimeError("reconnect changed client address")
            finally:
                if reconnect.poll() is None:
                    reconnect.kill()
                reconnect.communicate(timeout=5)
            watcher = subprocess.Popen(["target/debug/examples/headscale-probe", f"127.0.0.1:{port}", path("root.der"), str(auth), path("identity"), "watch-peers"], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            try:
                received = read_until(watcher, b"map ready for peers\n")
                peer = subprocess.run(["target/debug/examples/headscale-probe", f"127.0.0.1:{port}", path("root.der"), str(auth), path("peer-identity"), "register"], capture_output=True, text=True, timeout=30)
                if peer.returncode:
                    raise RuntimeError(f"peer registration failed: {peer.stderr.strip()}")
                peer_result = json.loads(next(line for line in peer.stdout.splitlines() if line.startswith("{")))
                received = read_until(watcher, f"peer authorized {peer_result['node_key']}\n".encode(), received)
                received = read_until(watcher, b"packet authorized before policy update\n", received)
                (folder / "policy.json").write_text(json.dumps({"acls": [{"action": "accept", "src": ["*"], "dst": ["*:1235"], "proto": "udp"}]}))
                run(command + ["policy", "set", "--file", path("policy.json")])
                received = read_until(watcher, b"packet revoked by policy update\n", received)
                nodes = json.loads(run(command + ["nodes", "list", "--output", "json"]).stdout)
                peers = [node for node in nodes if node["id"] != after[1]["id"]]
                if len(nodes) != 2 or len(peers) != 1:
                    raise RuntimeError("expected exactly one added peer")
                run(command + ["nodes", "delete", "--identifier", str(peers[0]["id"]), "--force"])
                stdout, stderr = watcher.communicate(timeout=30)
                print((received + stdout).decode(), end="", flush=True)
                if watcher.returncode:
                    raise RuntimeError(f"peer update probe failed: {stderr.decode().strip()}")
                incremental = json.loads(next(line for line in (received + stdout).decode().splitlines() if line.startswith("{")))
                if not all(incremental[name] for name in ["incremental_peer_added", "incremental_peer_removed", "local_authorization_retained", "policy_revocation", "policy_retained_port"]):
                    raise RuntimeError("incremental peer evidence missing")
            finally:
                if watcher.poll() is None:
                    watcher.kill()
                watcher.communicate(timeout=5)
            peer_version, tcp_measurements = independent_peer(folder, port, auth, command, after[0]["assigned_ipv4"])
            OUTPUT.parent.mkdir(exist_ok=True)
            OUTPUT.write_text(json.dumps({
                "verified_at": datetime.now(timezone.utc).isoformat(),
                "headscale": run(["headscale", "version"]).stdout.splitlines()[0],
                "capability_version": after[0]["capability_version"],
                "tls_untrusted_rejected": True, "tls_bootstrap": True, "noise_http2": True,
                "registration": True, "initial_map_authorization": True, "node_key_rotation": True,
                "expired_key_registration_denied": True, "expired_key_map_denied": True,
                "expired_key_renewal": True,
                "coordinator_restart_reconnected": True, "reconnect_fresh_registration": True,
                "reconnect_fresh_map_authorization": True, "reconnect_disconnected_access_denied": True,
                "reconnect_identity_record_retained": True,
                "endpoint_publication_before_restart": True, "endpoint_publication_after_restart": True,
                "active_map_expiry_delivered": True, "active_map_expiry_revoked": True,
                "incremental_peer_added": True, "incremental_peer_removed": True,
                "policy_update_revokes_queued_packet": True, "policy_update_denies_new_packet": True,
                "policy_update_retains_allowed_port": True,
                "coordinator_node_continuity": True, "assigned_ipv4": after[0]["assigned_ipv4"],
                "tailscale_peer": peer_version, "independent_peer_udp": True,
                "tcp_measurements": tcp_measurements,
                "independent_peer_tcp": True, "tcp_bytes_each_direction": 65536,
                "tcp_remote_eof": True, "tcp_orderly_close": True,
                "tcp_destination_only_policy": True,
                "tcp_renewal_beyond_original_expiry": True,
                "tcp_minimum_connection_ms": 195000,
                "tcp_minimum_authenticated_session_installations": 2,
                "independent_peer_tls_http": True, "tls_http_body_bytes": 65536,
                "tls_peer_wrong_name_rejected": True, "tls_peer_untrusted_rejected": True,
                "tls_peer_close_notify": True,
                "tls_peer_truncation_rejected": True,
                "peer_tls_server": ssl.OPENSSL_VERSION,
                "independent_opds_authenticated_acquisition": True,
                "opds_unauthenticated_denied": True, "opds_authenticated_redirect": True,
                "opds_chunked_feed": True, "opds_relative_acquisition": True,
                "acquired_epub_validated": True, "acquired_epub_exact": True, "acquired_epub_noclobber": True,
                "wireguard_peer_traffic": True, "derp_traffic": True, "derp_direct_udp_sent": 0, "derp_tls_verified": True, "derp_tcp": True, "derp_tcp_bytes_each_direction": 65536, "derp_tcp_orderly_close": True, "derp_tcp_transport_key_renewal_verified": True,
                "derp_opds_authenticated_acquisition": True,
                "derp_services_direct_udp_sent": 0,
                "derp_tls_http": True, "derp_tls_http_body_bytes": 65536,
                "derp_tls_wrong_name_rejected": True, "derp_tls_untrusted_rejected": True,
                "derp_tls_close_notify": True, "derp_tls_truncation_rejected": True,
                "derp_tcp_minimum_connection_ms": 195000, "derp_tcp_minimum_authenticated_session_installations": 2, "hardware_verified": False,
            }, indent=2) + "\n")
            print("Independent Headscale control, UDP/TCP/TLS, OPDS acquisition checks passed")
        finally:
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=5)
