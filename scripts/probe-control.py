"""Resolve the coordinator for the Rust HTTPS bootstrap and Noise probe."""
import socket
import subprocess
from pathlib import Path

host = "controlplane.tailscale.com"
addresses = socket.getaddrinfo(host, 443, socket.AF_INET, socket.SOCK_STREAM)
address = addresses[0][4][0] + ":443"
binary = Path(__file__).resolve().parent.parent / "target/debug/examples/control-probe"
subprocess.run([str(binary), host, address], check=True, timeout=40)
