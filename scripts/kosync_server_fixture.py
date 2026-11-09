"""Disposable official KOReader server, bound only to an ephemeral loopback port."""
import hashlib
import json
import subprocess
import time
import urllib.request

IMAGE = "docker.io/koreader/kosync@sha256:bb3f13615365703315a43b9059f65e71e876440f867e23a42bf27f2fa18264e1"

class OfficialServer:
    def __enter__(self):
        self.container = subprocess.check_output(["podman", "run", "--detach", "--rm", "--publish", "127.0.0.1::17200", "--env", "ENABLE_USER_REGISTRATION=true", IMAGE], text=True).strip()
        try:
            address = subprocess.check_output(["podman", "port", self.container, "17200/tcp"], text=True).strip()
            assert address.startswith("127.0.0.1:")
            self.url = "http://" + address
            deadline = time.monotonic() + 40
            while True:
                try:
                    request = urllib.request.Request(self.url + "/healthcheck", headers={"Accept":"application/vnd.koreader.v1+json"})
                    with urllib.request.urlopen(request, timeout=2) as response:
                        assert json.load(response)["state"] == "OK"
                    break
                except (OSError, ValueError):
                    if time.monotonic() >= deadline:
                        raise
                    time.sleep(0.2)
            controller = subprocess.check_output(["podman", "exec", self.container, "cat", "/app/koreader-sync-server/app/controllers/1/syncs_controller.lua"])
            self.controller_hash = hashlib.sha256(controller).hexdigest()
            return self
        except BaseException:
            self.__exit__(None, None, None)
            raise

    def __exit__(self, *_):
        subprocess.run(["podman", "stop", "--time", "3", self.container], check=False, stdout=subprocess.DEVNULL)
