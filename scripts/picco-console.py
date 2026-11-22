#!/usr/bin/env python3
"""Drive the Picco USB console without resetting the device."""
import argparse
import glob
import json
import os
import re
import select
import sys
import time
import tty
import zlib

CHUNK_BYTES = 2048
EVENTS = ["next", "prev", "select", "back", "power"]


class Port:
    def __init__(self, path, timeout):
        self.fd = os.open(path, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
        tty.setraw(self.fd)
        self.deadline = time.monotonic() + timeout
        self.pending = bytearray()
        # The device drops input sent immediately after the port opens. Drain
        # old output meanwhile so a previous reply cannot satisfy this request.
        settled = time.monotonic() + 0.3
        while select.select([self.fd], [], [], max(0, settled - time.monotonic()))[0]:
            if not os.read(self.fd, 4096):
                raise OSError("USB disconnected")

    def write(self, data):
        while data:
            remaining = self.deadline - time.monotonic()
            if remaining <= 0 or not select.select([], [self.fd], [], remaining)[1]:
                raise TimeoutError("USB write timed out")
            data = data[os.write(self.fd, data):]

    def lines(self, echo=True):
        """Yield complete device lines until the deadline."""
        while True:
            while b"\n" in self.pending:
                line, _, self.pending = self.pending.partition(b"\n")
                if echo:
                    print(line.decode("utf-8", "replace"), file=sys.stderr, flush=True)
                yield bytes(line)
            if len(self.pending) > 4096:
                raise ValueError("USB line exceeds 4096 bytes")
            remaining = self.deadline - time.monotonic()
            if remaining <= 0 or not select.select([self.fd], [], [], remaining)[0]:
                raise TimeoutError("No Femto reply received")
            data = os.read(self.fd, 4096)
            if not data:
                raise OSError("USB disconnected")
            self.pending.extend(data)

    def file_result(self, on_line=lambda line: None, echo=True):
        for line in self.lines(echo):
            if b"USB file: done" in line:
                return
            error = re.search(rb"USB file: error (.*)", line)
            if error:
                raise RuntimeError(error[1].decode("utf-8", "replace").strip())
            on_line(line)


def status(port, expect_page):
    memory = {}
    for line in port.lines():
        stack = re.search(rb"Stack usb status: minimum_free=(\d+)", line)
        if stack:
            memory["stack_minimum_free"] = int(stack[1])
        heap = re.search(rb"Heap usb status (internal|psram): free=(\d+) largest=(\d+) minimum=(\d+)", line)
        if heap:
            memory[heap[1].decode("ascii")] = dict(
                free=int(heap[2]), largest=int(heap[3]), minimum=int(heap[4]))
        match = re.search(rb"USB status: page=(\d+) pages=(\d+) menu=(true|false)", line)
        if match:
            result = dict(page=int(match[1]), pages=int(match[2]), menu=match[3] == b"true")
            result["memory_bytes"] = memory
            print(json.dumps(result))
            return 0 if expect_page is None or result["page"] == expect_page else 1


def put(port, source, destination):
    with open(source, "rb") as file:
        data = file.read()
    port.write(f"femto put {len(data)} {zlib.crc32(data):08x} {destination}\n".encode())

    def acknowledge(line):
        ack = re.search(rb"USB put: ack (\d+)", line)
        if ack:
            port.deadline = time.monotonic() + 30
            port.write(data[int(ack[1]):int(ack[1]) + CHUNK_BYTES])

    started = time.monotonic()
    port.file_result(acknowledge, echo=False)
    print(json.dumps(dict(path=destination, bytes=len(data),
                          seconds=round(time.monotonic() - started, 2))))


def listing(port, directory):
    entries = []

    def collect(line):
        entry = re.search(rb"USB ls: ([df]) (\d+) (.*)", line)
        if entry:
            entries.append(dict(kind="directory" if entry[1] == b"d" else "file",
                                bytes=int(entry[2]),
                                name=entry[3].decode("utf-8", "replace").rstrip("\r")))

    port.write(f"femto ls {directory}".rstrip().encode() + b"\n")
    port.file_result(collect, echo=False)
    print(json.dumps(entries, indent=1, ensure_ascii=False))


def frame(port, output, rotate):
    """Save the framebuffer; it is what the firmware drew, not a photo of the glass."""
    from PIL import Image
    # Log lines from other device threads can interleave with the dump, so
    # only complete rows count and a damaged dump is requested again.
    for _ in range(4):
        port.write(b"femto frame\n")
        data, size = bytearray(), None
        for line in port.lines(echo=False):
            begin = re.search(rb"USB frame: begin (\d+)", line)
            if begin:
                size = int(begin[1])
            elif b"USB frame: end" in line:
                break
            elif size is not None:
                row = re.fullmatch(rb"USB frame: ((?:[0-9a-f]{2}){1,64})\r?", line)
                if row:
                    data.extend(bytes.fromhex(row[1].decode("ascii")))
        if size in (48000, 96000) and len(data) == size:
            break
    else:
        raise ValueError(f"incomplete frame: {len(data)} of {size} bytes")
    if size == 48000:
        image = Image.frombytes("1", (800, 480), bytes(data))
    else:
        high = Image.frombytes("1", (800, 480), bytes(data[:48000])).convert("L")
        low = Image.frombytes("1", (800, 480), bytes(data[48000:])).convert("L")
        image = Image.blend(low, high, 2 / 3)
    image.rotate(rotate, expand=True).save(output)
    print(json.dumps(dict(output=output, bytes=size)))


def follow_recovery(path, timeout=300):
    """Report the stock recovery application's installation until it finishes."""
    deadline = time.monotonic() + timeout
    pending = b""
    while time.monotonic() < deadline:
        # The port disappears while the device restarts into recovery.
        try:
            fd = os.open(path, os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK)
        except OSError:
            time.sleep(0.1)
            continue
        try:
            tty.setraw(fd)
            while time.monotonic() < deadline:
                if not select.select([fd], [], [], 0.2)[0]:
                    continue
                data = os.read(fd, 4096)
                if not data:
                    break
                *lines, pending = (pending + data).split(b"\n")
                for line in lines:
                    text = line.decode("utf-8", "replace").strip()
                    if "[SD_OTA]" in text or "[RECOVERY_BOOT]" in text:
                        print(text, file=sys.stderr, flush=True)
                    if "install ok" in text:
                        return
                    if "install failed" in text or "auto install deferred" in text:
                        raise RuntimeError(text)
        except OSError:
            pass
        finally:
            os.close(fd)
    raise TimeoutError("recovery did not report an installation")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port")
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--expect-page", type=int)
    commands = parser.add_subparsers(dest="command", required=True)
    for name in ["status", "reboot", "awake", *EVENTS]:
        commands.add_parser(name)
    commands.add_parser("swipe").add_argument("direction", choices=["next", "prev"])
    commands.add_parser("type", help="enter text into the open keyboard field").add_argument("text")
    commands.add_parser("choose", help="activate the menu entry whose label starts with the text").add_argument("text")
    tap = commands.add_parser("tap", help="tap at reader-view coordinates")
    tap.add_argument("x", type=int)
    tap.add_argument("y", type=int)
    upload = commands.add_parser("put", help="copy a file below /sdcard/femto")
    upload.add_argument("source")
    upload.add_argument("destination")
    commands.add_parser(
        "update", help="copy a firmware image to the card and install it through recovery"
    ).add_argument("image")
    commands.add_parser("ls").add_argument("directory", nargs="?", default="")
    commands.add_parser("rm").add_argument("path")
    capture = commands.add_parser("frame", help="save the framebuffer as an upright PNG")
    capture.add_argument("output")
    capture.add_argument("--rotate", type=int, default=270, choices=[0, 90, 180, 270],
                         help="counterclockwise degrees from native panel orientation")
    args = parser.parse_args()
    if not 0 < args.timeout <= 300:
        parser.error("timeout must be between 0 and 300 seconds")
    path = args.port
    if path is None:
        found = glob.glob("/dev/serial/by-id/usb-Espressif_USB_JTAG_serial_debug_unit_*-if00")
        if len(found) != 1:
            parser.error("--port is required unless exactly one Picco is connected")
        path = found[0]
    port = Port(path, args.timeout)
    try:
        if args.command == "put":
            put(port, args.source, args.destination)
        elif args.command == "update":
            put(port, args.image, "firmware.bin")
            port.write(b"femto update\n")
            follow_recovery(path)
            print(json.dumps(dict(installed=args.image)))
        elif args.command == "ls":
            listing(port, args.directory)
        elif args.command == "rm":
            port.write(f"femto rm {args.path}\n".encode())
            port.file_result(echo=False)
        elif args.command == "frame":
            frame(port, args.output, args.rotate)
        elif args.command in ("reboot", "awake"):
            port.write(f"femto {args.command}\n".encode())
        else:
            commands = f"femto {args.command}\n"
            if args.command == "tap":
                commands = f"femto tap {args.x} {args.y}\n"
            if args.command == "swipe":
                commands = f"femto swipe {args.direction}\n"
            if args.command in ("type", "choose"):
                commands = f"femto {args.command} {args.text}\n"
            if args.command != "status":
                commands += "femto status\n"
            port.write(commands.encode("ascii"))
            return status(port, args.expect_page)
        return 0
    finally:
        os.close(port.fd)


if __name__ == "__main__":
    raise SystemExit(main())
