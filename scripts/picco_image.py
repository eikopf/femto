"""Bounded structural validation of unsigned ESP32-S3 application images."""
import hashlib
import struct


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def image_info(data, *, exact=False, application=True):
    require(len(data) >= 24 and data[0] == 0xE9, "invalid ESP image header")
    require(1 <= data[1] <= 16, "invalid image segment count")
    require(struct.unpack_from("<H", data, 12)[0] == 9, "image is not ESP32-S3")
    require(data[23] in (0, 1), "invalid appended hash flag")
    cursor, checksum = 24, 0xEF
    for index in range(data[1]):
        require(cursor + 8 <= len(data), "truncated segment header")
        _, length = struct.unpack_from("<II", data, cursor)
        cursor += 8
        require(length <= len(data) - cursor, "truncated image segment")
        if index == 0 and application:
            require(length >= 256 and struct.unpack_from("<I", data, cursor)[0] == 0xABCD5432,
                    "application descriptor missing")
        for byte in data[cursor:cursor + length]:
            checksum ^= byte
        cursor += length
    checksum_at = (cursor // 16 + 1) * 16 - 1
    require(checksum_at < len(data), "image checksum missing")
    require(not any(data[cursor:checksum_at]), "invalid image padding")
    require(data[checksum_at] == checksum, "image checksum mismatch")
    end = checksum_at + 1
    if data[23]:
        require(end + 32 <= len(data), "image digest missing")
        require(hashlib.sha256(data[:end]).digest() == data[end:end + 32], "image SHA-256 mismatch")
        end += 32
    if exact:
        require(data[23] == 1, "candidate must have an appended SHA-256")
        require(end == len(data), "candidate contains trailing data or unsupported signature")
    return {"bytes": end, "sha256": sha(data[:end]), "appended_digest": bool(data[23])}

