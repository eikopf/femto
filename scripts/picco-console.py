#!/usr/bin/env python3
"""Send a bounded USB reader command without resetting the Picco."""
import argparse
import json
import os
import re
import select
import time
import tty


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", required=True)
    parser.add_argument("command", choices=["status", "next", "prev", "select", "back"])
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--expect-page", type=int)
    args = parser.parse_args()
    if not 0 < args.timeout <= 300:
        parser.error("timeout must be between 0 and 300 seconds")
    fd = os.open(args.port, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
    try:
        tty.setraw(fd)
        # Drain old output so a previous status cannot satisfy this request.
        while select.select([fd], [], [], 0)[0]:
            if not os.read(fd, 4096):
                raise OSError("USB disconnected")
        commands = f"femto {args.command}\n"
        if args.command != "status":
            commands += "femto status\n"
        data = commands.encode("ascii")
        deadline = time.monotonic() + args.timeout
        while data:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([], [fd], [], remaining)[1]:
                raise TimeoutError("USB write timed out")
            data = data[os.write(fd, data):]
        pending = bytearray()
        memory = {}
        while time.monotonic() < deadline:
            if not select.select([fd], [], [], max(0, deadline - time.monotonic()))[0]:
                break
            data = os.read(fd, 4096)
            if not data:
                raise OSError("USB disconnected")
            pending.extend(data)
            while b"\n" in pending:
                line, _, pending = pending.partition(b"\n")
                stack = re.search(rb"Stack usb status: minimum_free=(\d+)", line)
                if stack:
                    memory["stack_minimum_free"] = int(stack[1])
                heap = re.search(rb"Heap usb status (internal|psram): free=(\d+) largest=(\d+) minimum=(\d+)", line)
                if heap:
                    memory[heap[1].decode("ascii")] = dict(
                        free=int(heap[2]), largest=int(heap[3]), minimum=int(heap[4]))
                match = re.search(rb"USB status: page=(\d+) pages=(\d+) menu=(true|false)", line)
                if match:
                    status = dict(page=int(match[1]), pages=int(match[2]), menu=match[3] == b"true")
                    status["memory_bytes"] = memory
                    print(json.dumps(status))
                    return 0 if args.expect_page is None or status["page"] == args.expect_page else 1
            if len(pending) > 4096:
                raise ValueError("USB line exceeds 4096 bytes")
        raise TimeoutError("No Femto status received")
    finally:
        os.close(fd)


if __name__ == "__main__":
    raise SystemExit(main())
