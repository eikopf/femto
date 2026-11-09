"""Exercise the Femto CLI against an isolated official KOReader service."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import urllib.request


def check(url):
    subprocess.run(["cargo", "build", "--locked", "-p", "femto", "--bin", "femto"], check=True)
    book = Path("tests/fixtures/sample.epub").resolve()
    book_id = hashlib.sha256(book.read_bytes()).hexdigest()
    username = "femto-cli-fixture-" + str(os.getpid())
    password = "isolated-cli-fixture-password"
    with tempfile.TemporaryDirectory(prefix="femto-kosync-cli-") as folder:
        folder = Path(folder)
        config = folder / "kosync.json"
        config_data = {"server": url, "username": username, "password": password, "allow_http": True, "device_id": "cli-fixture"}
        config.write_text(json.dumps(config_data))
        config.chmod(0o600)
        state = folder / "reader.json"
        bookmark = {"label": "Keep this place", "location": {"chapter": 0, "block": 1, "byte": 3}}
        state.write_text(json.dumps({"version":1, "layout":{"width":480,"height":800,"margin":24,"font_size":24,"line_height":1.35},"books":{book_id:{"position":bookmark["location"],"bookmarks":[bookmark],"last_opened":123}}}))

        def cli(action, success=True):
            args = ["target/debug/femto", "kosync", "--config", str(config), action]
            if action in ("push", "pull", "status"):
                args += [str(book), "--state", str(state)]
            result = subprocess.run(args, capture_output=True, text=True, timeout=30)
            assert (result.returncode == 0) == success, result.stderr
            assert password not in result.stdout + result.stderr
            return json.loads(result.stdout) if success else None

        def peer(method, path, body=None):
            request = urllib.request.Request(url + path, method=method,
                data=None if body is None else json.dumps(body).encode(),
                headers={"Accept":"application/vnd.koreader.v1+json", "Content-Type":"application/json", "x-auth-user":username, "x-auth-key":hashlib.md5(password.encode()).hexdigest()})
            with urllib.request.urlopen(request, timeout=5) as response:
                return json.load(response)

        assert cli("register")["status"] == "registered"
        assert cli("auth")["status"] == "authenticated"
        before = state.read_bytes()
        uploaded = cli("push")
        assert state.read_bytes() == before
        document = uploaded["document"]
        remote = peer("GET", "/syncs/progress/" + document)
        assert remote["progress"] == "/body[1]/DocFragment[1]/body[1]/p[1]/text()[1].3"
        assert remote["device_id"] == "cli-fixture"
        peer("PUT", "/syncs/progress", {"document":document,"progress":"/body/DocFragment[1]/body/h1/text().5","percentage":0.1,"device":"Independent fixture","device_id":"peer-fixture"})
        assert cli("pull")["status"] == "downloaded"
        saved = json.loads(state.read_text())["books"][book_id]
        assert saved["position"] == {"chapter":0,"block":0,"byte":5}
        assert saved["bookmarks"] == [bookmark]
        before = state.read_bytes()
        assert cli("status")["device"] == "Independent fixture"
        assert state.read_bytes() == before
        config_data["password"] = "wrong"
        config.write_text(json.dumps(config_data))
        cli("pull", success=False)
        assert state.read_bytes() == before
        config_data["password"] = password
        config_data["checksum"] = "filename"
        config.write_text(json.dumps(config_data))
        assert cli("pull")["status"] == "no_remote_progress"
        assert state.read_bytes() == before
        config_data["checksum"] = "binary"
        config.write_text(json.dumps(config_data))
        book = Path("tests/fixtures/kosync-images.epub").resolve()
        book_id = hashlib.sha256(book.read_bytes()).hexdigest()
        saved = json.loads(state.read_text())
        previous_books = saved["books"].copy()
        image_position = {"chapter":1,"block":0,"byte":0}
        saved["books"][book_id] = {"position":image_position,"bookmarks":[],"last_opened":124}
        state.write_text(json.dumps(saved))
        document = cli("push")["document"]
        assert peer("GET", "/syncs/progress/" + document)["progress"] == "/body[1]/DocFragment[2]/body[1]/img[1]"
        peer("PUT", "/syncs/progress", {"document":document,"progress":"/body/DocFragment[1]/body/p/img[2]","percentage":0.2,"device":"Independent fixture","device_id":"peer-fixture"})
        assert cli("pull")["status"] == "downloaded"
        saved = json.loads(state.read_text())["books"]
        assert saved[book_id]["position"] == {"chapter":0,"block":2,"byte":0}
        assert all(saved[key] == value for key, value in previous_books.items())
    return {"cli_registration_auth_push_pull_status":True,"independent_peer_exchange":True,"bookmarks_preserved":True,"failed_and_empty_pull_preserves_state":True,"image_positions_and_other_books_preserved":True}


def check_tailnet(runtime, services, folder):
    from kosync_server_fixture import OfficialServer, IMAGE
    with OfficialServer() as server:
        services.kosync_origin = server.url
        try:
            config = folder / "tailnet-kosync.json"
            username, password = "tailnet-fixture", "private-tailnet-fixture-password"
            config_data = {"server":services.origin,"username":username,"password":password,"device_id":"tailnet-cli","route":"tailnet"}
            config.write_text(json.dumps(config_data))
            config.chmod(0o600)
            book = folder / "acquired.epub"
            book_id = hashlib.sha256(book.read_bytes()).hexdigest()
            state = folder / "kosync-state.json"
            bookmark = {"label":"Keep bookmark","location":{"chapter":0,"block":1,"byte":3}}
            state.write_text(json.dumps({"version":1,"layout":{"width":480,"height":800,"margin":24,"font_size":24,"line_height":1.35},"books":{book_id:{"position":bookmark["location"],"bookmarks":[bookmark],"last_opened":123}}}))

            def cli(action, success=True):
                args = runtime + ["--kosync-config",config,"--kosync-action",action]
                if action in ("push","pull","status"):
                    args += ["--kosync-book",book,"--kosync-state",state]
                result = subprocess.run(list(map(str,args)),capture_output=True,text=True,timeout=60)
                assert (result.returncode == 0) == success, result.stderr
                assert password not in result.stdout + result.stderr
                if not success:
                    return result.stderr
                text = result.stdout.strip()
                decoder = json.JSONDecoder()
                while text:
                    last, end = decoder.raw_decode(text)
                    text = text[end:].lstrip()
                return last

            assert cli("register")["status"] == "registered"
            assert cli("auth")["status"] == "authenticated"
            before = state.read_bytes()
            uploaded = cli("push")
            assert state.read_bytes() == before
            document = uploaded["document"]
            headers = {"Accept":"application/vnd.koreader.v1+json","Content-Type":"application/json","x-auth-user":username,"x-auth-key":hashlib.md5(password.encode()).hexdigest()}
            with urllib.request.urlopen(urllib.request.Request(server.url + "/syncs/progress/" + document,headers=headers),timeout=5) as response:
                assert json.load(response)["progress"] == "/body[1]/DocFragment[1]/body[1]/p[1]/text()[1].3"
            body = {"document":document,"progress":"/body/DocFragment[1]/body/h1/text().4","percentage":0.1,"device":"Independent fixture","device_id":"other-device"}
            with urllib.request.urlopen(urllib.request.Request(server.url + "/syncs/progress",method="PUT",headers=headers,data=json.dumps(body).encode()),timeout=5) as response:
                assert response.status == 200
            assert cli("pull")["status"] == "downloaded"
            saved = json.loads(state.read_text())["books"][book_id]
            assert saved["position"] == {"chapter":0,"block":0,"byte":4}
            assert saved["bookmarks"] == [bookmark]
            before = state.read_bytes()
            config_data["password"] = "wrong"
            config.write_text(json.dumps(config_data))
            assert "authentication failed" in cli("pull",False)
            assert state.read_bytes() == before
            config_data["password"] = password
            config.write_text(json.dumps(config_data))
            for mode, expected in [("wrong-name","not valid for name"),("untrusted","UnknownIssuer")]:
                services.tls_mode = mode
                count = services.kosync_requests
                assert expected in cli("auth",False)
                assert services.kosync_requests == count
            return {"official_server_image":IMAGE,"controller_sha256":server.controller_hash,"registration_auth_push_pull":True,"independent_peer_exchange":True,"bookmarks_preserved":True,"authentication_and_tls_rejection":True,"requests":services.kosync_requests}
        finally:
            services.kosync_origin = None
            services.tls_mode = "valid"
