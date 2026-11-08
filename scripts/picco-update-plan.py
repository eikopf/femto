#!/usr/bin/env python3
"""Validate an offline Picco update against a full backup; never access a device."""
import argparse
import hashlib
import json
from pathlib import Path
import struct
import sys

from picco_image import image_info, require, sha

FLASH_BYTES = 16 * 1024 * 1024
TABLE_OFFSET = 0x8000
SECTOR = 4096


def read_bounded(path, maximum):
    with Path(path).open("rb") as source:
        data = source.read(maximum + 1)
    require(len(data) <= maximum, "input exceeds size limit")
    return data



def partitions(backup):
    table = backup[TABLE_OFFSET:TABLE_OFFSET + SECTOR]
    require(len(table) == SECTOR, "partition table sector missing")
    entries, names = [], set()
    checksum_found = False
    for cursor in range(0, 0xC00, 32):
        record = table[cursor:cursor + 32]
        if record[:16] == b"\xeb\xeb" + b"\xff" * 14:
            require(entries and hashlib.md5(table[:cursor]).digest() == record[16:],
                    "partition table MD5 mismatch")
            require(table[cursor + 32:] == b"\xff" * (SECTOR - cursor - 32),
                    "unsupported partition table trailing data")
            checksum_found = True
            break
        require(record[:2] == b"\xaa\x50", "partition entry or MD5 missing")
        _, kind, subtype, offset, size, raw_name, flags = struct.unpack("<HBBII16sI", record)
        label = raw_name.split(b"\x00", 1)[0]
        require(label and all(32 < byte < 127 for byte in label), "invalid partition label")
        name = label.decode("ascii")
        require(name not in names, "duplicate partition label")
        names.add(name)
        require(kind in (0, 1) and flags == 0, "unsupported partition type or flags")
        require(size > 0 and offset >= TABLE_OFFSET + SECTOR and offset + size <= len(backup),
                "partition outside backup or overlaps bootloader/table")
        require(offset % (0x10000 if kind == 0 else SECTOR) == 0 and size % SECTOR == 0,
                "partition alignment")
        require(all(offset + size <= old["offset"] or offset >= old["offset"] + old["bytes"]
                    for old in entries), "overlapping partitions")
        entries.append({"name": name, "type": kind, "subtype": subtype, "offset": offset,
                        "bytes": size, "sha256": sha(backup[offset:offset + size])})
    require(checksum_found, "partition table has no verified MD5")
    return entries


def plan(backup, image, manifest):
    require(len(backup) == FLASH_BYTES, "Picco backup must contain exactly 16 MiB from flash offset zero")
    candidate = image_info(image, exact=True)
    require(manifest.get("chip") == "esp32s3" and manifest.get("bytes") == len(image)
            and manifest.get("sha256") == sha(image), "candidate manifest mismatch")
    table = partitions(backup)
    apps = [entry for entry in table if entry["name"] == "app" and entry["type"] == 0 and entry["subtype"] == 0x10]
    recoveries = [entry for entry in table if entry["type"] == 0 and entry["subtype"] == 0]
    require(len(apps) == 1 and len(recoveries) == 1, "requires app ota_0 and one factory recovery partition")
    app, recovery = apps[0], recoveries[0]
    require(app["offset"] == 0xE0000 and app["bytes"] == 0xE70000,
            "application slot differs from the compiled Picco reference profile")
    require(recovery["name"] == "safeboot" and recovery["offset"] == 0x20000 and recovery["bytes"] == 0xC0000,
            "recovery slot differs from the Picco reference profile")
    require(manifest.get("reference_app_offset") == "0xE0000"
            and manifest.get("reference_app_size") == "0xE70000", "manifest slot mismatch")
    require(len(image) <= app["bytes"], "candidate exceeds application slot")
    bootloader = image_info(backup[:TABLE_OFFSET], application=False)
    recovery_image = image_info(backup[recovery["offset"]:recovery["offset"] + recovery["bytes"]])
    previous_image = image_info(backup[app["offset"]:app["offset"] + app["bytes"]])
    erase_bytes = (len(image) + SECTOR - 1) // SECTOR * SECTOR
    end = app["offset"] + erase_bytes
    return {
        "version": 1, "kind": "offline-picco-update-plan", "write_authorized": False,
        "hardware_verified": False, "security_state": "not established by flash backup",
        "backup": {"bytes": len(backup), "sha256": sha(backup)},
        "partition_table": {"offset": TABLE_OFFSET, "bytes": SECTOR,
                            "sha256": sha(backup[TABLE_OFFSET:TABLE_OFFSET + SECTOR])},
        "partitions": table, "bootloader_image": bootloader,
        "recovery_image": recovery_image, "previous_application_image": previous_image,
        "candidate": candidate,
        "proposed_write": {"offset": app["offset"], "bytes": len(image), "sha256": sha(image)},
        "proposed_erase": {"offset": app["offset"], "bytes": erase_bytes},
        "preserve": [{"offset": 0, "bytes": app["offset"], "sha256": sha(backup[:app["offset"]])},
                     {"offset": end, "bytes": len(backup) - end, "sha256": sha(backup[end:])}],
        "rollback": {"offset": app["offset"], "bytes": app["bytes"], "sha256": app["sha256"]},
        "remaining_gates": ["device identity and backup provenance", "live eFuse security and chip revision",
                            "boot-selection metadata and tested factory recovery entry",
                            "post-write readback and physical boot/recovery qualification"],
    }


def verify_readback(backup, image, manifest, readback):
    result = plan(backup, image, manifest)
    require(len(readback) == FLASH_BYTES, "readback must contain exactly 16 MiB from flash offset zero")
    start = result["proposed_write"]["offset"]
    image_end = start + len(image)
    erase_end = start + result["proposed_erase"]["bytes"]
    ranges = [("candidate", start, image_end, image),
              ("erased_tail", image_end, erase_end, b"\xff" * (erase_end - image_end))]
    ranges += [("preserved_prefix", 0, start, backup[:start]),
               ("preserved_suffix", erase_end, len(backup), backup[erase_end:])]
    checks = []
    for name, begin, end, expected in ranges:
        actual = readback[begin:end]
        mismatch = None
        if actual != expected:
            mismatch = begin + next(index for index, (left, right) in enumerate(zip(actual, expected))
                                    if left != right)
        checks.append({"name": name, "offset": begin, "bytes": end - begin,
                       "expected_sha256": sha(expected), "actual_sha256": sha(actual),
                       "matches": mismatch is None, "first_mismatch_offset": mismatch})
    result["kind"] = "offline-picco-readback-verification"
    result["readback"] = {"bytes": len(readback), "sha256": sha(readback),
                          "matches_plan": all(check["matches"] for check in checks), "checks": checks}
    result["remaining_gates"][-1] = "readback provenance and physical boot/recovery qualification"
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--backup", type=Path, required=True)
    parser.add_argument("--image", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--readback", type=Path, help="verify a complete post-write flash dump against the inputs")
    args = parser.parse_args()
    try:
        manifest = json.loads(read_bounded(args.manifest, 16 * 1024))
        require(isinstance(manifest, dict), "manifest must be an object")
        require(manifest.get("image") == args.image.name, "manifest image filename mismatch")
        backup = read_bounded(args.backup, FLASH_BYTES)
        image = read_bounded(args.image, FLASH_BYTES)
        if args.readback:
            result = verify_readback(backup, image, manifest, read_bounded(args.readback, FLASH_BYTES))
        else:
            result = plan(backup, image, manifest)
    except (ValueError, OSError, struct.error) as error:
        print(f"Update plan rejected: {error}", file=sys.stderr)
        return 1
    print(json.dumps(result, indent=2))
    return 0 if result.get("readback", {}).get("matches_plan", True) else 1


if __name__ == "__main__":
    sys.exit(main())
