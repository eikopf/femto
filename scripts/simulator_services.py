"""Private official Tailscale peer and authenticated native-reader service fixture."""
import base64
import hashlib
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import os
from pathlib import Path
import secrets
import ssl
import subprocess
import threading
import time


class PeerServices:
    def __init__(self, folder, coordinator_port, admin, local_address, https=False):
        self.folder, self.coordinator_port, self.admin = folder, coordinator_port, admin
        self.local_address = local_address
        self.https = https
        self.tls_mode = "valid"
        self.tls_rejections = {"wrong-name": 0, "untrusted": 0}
        self.tls_failures = []
        self.daemon = self.log = self.server = self.thread = None
        self.counts = {"feed": 0, "book": 0, "gets": 0, "puts": 0}
        self.feed_received_at = []
        self.remote = {"record": None, "etag": 0}
        self.errors = []
        self.progress_gate = None

    def run(self, args, **kwargs):
        return subprocess.run(args, check=True, capture_output=True, text=True, timeout=30, **kwargs)

    def __enter__(self):
        try:
            return self.start()
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def start(self):
        socket = self.folder / "native-tailscaled.sock"
        env = dict(os.environ, SSL_CERT_FILE=str(self.folder / "root.pem"), TS_DISABLE_PORTMAPPER="true", TS_NO_LOGS_NO_SUPPORT="true")
        self.log = (self.folder / "native-tailscaled.log").open("w+")
        self.daemon = subprocess.Popen(["tailscaled", "--tun=userspace-networking", "--socket=" + str(socket), "--state=" + str(self.folder / "native-peer.state"), "--statedir=" + str(self.folder / "native-peer"), "--port=0", "--no-logs-no-support"], stdout=self.log, stderr=self.log, env=env)
        deadline = time.monotonic() + 10
        while not socket.exists():
            if self.daemon.poll() is not None or time.monotonic() >= deadline:
                raise RuntimeError("private native-service peer startup failed")
            time.sleep(0.05)
        cli = ["tailscale", "--socket", str(socket)]
        self.run(cli + ["up", f"--login-server=https://localhost:{self.coordinator_port}", "--auth-key=file:" + str(self.folder / "auth.key"), "--hostname=femto-native-service", "--accept-dns=false", "--accept-routes=false", "--netfilter-mode=off", "--timeout=20s"], env=env)
        status = json.loads(self.run(cli + ["status", "--json"], env=env).stdout)
        self.peer_version = self.run(["tailscale", "version"]).stdout.splitlines()[0]
        dns = status["Self"]["DNSName"].rstrip(".")
        peer_address = next(address for address in status["Self"]["TailscaleIPs"] if ":" not in address)
        password = secrets.token_hex(24)
        expected_auth = "Basic " + base64.b64encode(f"fixture:{password}".encode()).decode()
        epub = Path("tests/fixtures/sample.epub").read_bytes()
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, *_):
                pass

            def reply(self, status, body=b"", media="application/json", etag=None):
                self.send_response(status)
                self.send_header("Content-Type", media)
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Connection", "close")
                if etag is not None:
                    self.send_header("ETag", f'"{etag}"')
                self.end_headers()
                self.wfile.write(body)
                self.close_connection = True

            def authorized(self):
                if fixture.https and self.connection.selected_alpn_protocol() != "http/1.1":
                    fixture.errors.append("service TLS ALPN mismatch")
                    self.reply(400)
                    return False
                if self.headers.get("Host") != fixture.authority or self.client_address[0] != "127.0.0.1":
                    fixture.errors.append("unexpected service authority or peer source")
                    self.reply(400)
                    return False
                if self.headers.get("Authorization") != expected_auth:
                    fixture.errors.append("missing service authentication")
                    self.reply(401)
                    return False
                return True

            def do_GET(self):
                if not self.authorized():
                    return
                if self.path == "/feed":
                    fixture.feed_received_at.append(time.monotonic())
                    fixture.counts["feed"] += 1
                    body = {"metadata": {"title": "Tailnet native catalog"}, "links": [{"rel": "self", "href": "/feed", "type": "application/opds+json"}], "publications": [{"metadata": {"title": "Tailnet fixture"}, "links": [{"rel": "http://opds-spec.org/acquisition/open-access", "href": "/book.epub", "type": "application/epub+zip"}]}]}
                    self.reply(200, json.dumps(body).encode(), "application/opds+json")
                elif self.path == "/book.epub":
                    fixture.counts["book"] += 1
                    self.reply(200, epub, "application/epub+zip")
                elif self.path.startswith("/progress/"):
                    fixture.counts["gets"] += 1
                    gate = fixture.progress_gate
                    if gate is not None:
                        entered, release = gate
                        entered.set()
                        if not release.wait(20):
                            fixture.errors.append("held progress response was not released")
                        self.close_connection = True
                        return
                    record = fixture.remote["record"]
                    if record is None:
                        self.reply(404)
                    else:
                        self.reply(200, json.dumps(record).encode(), etag=fixture.remote["etag"])
                else:
                    self.reply(404)

            def do_PUT(self):
                if not self.authorized():
                    return
                if not self.path.startswith("/progress/") or fixture.remote["record"] is not None or self.headers.get("If-None-Match") != "*":
                    fixture.errors.append("unexpected progress write or precondition")
                    self.reply(412)
                    return
                record = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                if not self.path.endswith(record["book_id"]):
                    fixture.errors.append("progress book mismatch")
                    self.reply(400)
                    return
                fixture.remote["record"] = record
                fixture.remote["etag"] += 1
                fixture.counts["puts"] += 1
                self.reply(200, etag=fixture.remote["etag"])

        if self.https:
            contexts = {}
            for mode, name, ca in [("valid", dns, "root"), ("wrong-name", "wrong.example", "root"), ("untrusted", dns, "wrong")]:
                prefix = self.folder / ("native-service-" + mode)
                self.run(["openssl", "req", "-new", "-newkey", "rsa:2048", "-nodes", "-subj", f"/CN={name}", "-addext", f"subjectAltName=DNS:{name}", "-keyout", str(prefix) + ".key", "-out", str(prefix) + ".csr"])
                self.run(["openssl", "x509", "-req", "-in", str(prefix) + ".csr", "-CA", str(self.folder / (ca + ".pem")), "-CAkey", str(self.folder / (ca + ".key")), "-CAcreateserial", "-days", "1", "-copy_extensions", "copy", "-out", str(prefix) + ".pem"])
                context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
                context.load_cert_chain(str(prefix) + ".pem", str(prefix) + ".key")
                context.set_alpn_protocols(["http/1.1"])
                contexts[mode] = context

            class TLSService(ThreadingHTTPServer):
                def get_request(self):
                    connection, address = super().get_request()
                    connection.settimeout(10)
                    mode = fixture.tls_mode
                    try:
                        return contexts[mode].wrap_socket(connection, server_side=True), address
                    except (ssl.SSLError, ConnectionResetError, TimeoutError) as error:
                        reason = getattr(error, "reason", None) or type(error).__name__
                        fixture.tls_failures.append({"mode": mode, "reason": reason})
                        if mode in fixture.tls_rejections:
                            fixture.tls_rejections[mode] += 1
                        else:
                            fixture.errors.append("unexpected service TLS failure: " + reason)
                        connection.close()
                        raise
                    except BaseException:
                        connection.close()
                        raise
            self.server = TLSService(("127.0.0.1", 0), Handler)
        else:
            self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.authority = f"{dns}:{self.server.server_port}"
        self.origin = f"{'https' if self.https else 'http'}://{self.authority}"
        policy = self.folder / "native-services-policy.json"
        policy.write_text(json.dumps({"acls": [{"action": "accept", "src": [self.local_address], "dst": [f"{peer_address}:{self.server.server_port}"], "proto": "tcp"}]}))
        self.run(self.admin + ["policy", "set", "--file", str(policy)])
        self.books = self.folder / "native-books"
        self.books.mkdir()
        (self.books / "catalog.url").write_text(self.origin + "/feed")
        (self.books / "catalog-tailnet-origins").write_text(self.origin + "\n")
        (self.books / "progress.url").write_text(self.origin + "/progress/")
        (self.books / "progress-route").write_text("tailnet")
        for name in ["catalog-auth.json", "progress-auth.json"]:
            path = self.books / name
            path.write_text(json.dumps({"username": "fixture", "password": password, "allow_http": not self.https}))
            path.chmod(0o600)
        self.acquired = self.books / ("book-" + hashlib.sha256(epub).hexdigest() + ".epub")
        self.epub = epub
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()
        return self

    def __exit__(self, *_):
        if self.server is not None:
            if self.thread is not None:
                self.server.shutdown()
            self.server.server_close()
        if self.thread is not None:
            self.thread.join(timeout=5)
        if self.daemon is not None:
            self.daemon.terminate()
            try:
                self.daemon.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.daemon.kill()
                self.daemon.wait(timeout=5)
        if self.log is not None:
            self.log.close()
