"""Qualify the protocol and CLI against an isolated official KOReader server."""
import json
import os
from pathlib import Path
import subprocess
from kosync_cli_fixture import check as check_cli
from kosync_ui_fixture import check as check_ui
from kosync_server_fixture import OfficialServer, IMAGE

root = Path(__file__).resolve().parent.parent
os.chdir(root)
report = root / "output/kosync-server.json"
report.parent.mkdir(exist_ok=True)
report.unlink(missing_ok=True)
with OfficialServer() as server:
    env = dict(os.environ, FEMTO_KOSYNC_TEST_URL=server.url)
    subprocess.run(["cargo", "test", "--locked", "-p", "femto-kosync", "--test", "official_server", "--", "--ignored"], check=True, env=env)
    cli = check_cli(server.url)
    ui = check_ui(server.url)
    report.write_text(json.dumps({
        "image": IMAGE, "controller_sha256": server.controller_hash,
        "registration_auth_missing_progress_update_and_rejection": True,
        "cli": cli,
        "simulator": ui,
        "reader_position_interoperability": False,
        "tailnet_transport_tested": False,
        "hardware_verified": False,
    }, indent=2) + "\n")
