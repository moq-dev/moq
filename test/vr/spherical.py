"""Mark an MP4's video as VR180 left-right with Spherical Video V2 boxes.

ffmpeg reads and remuxes `st3d` and `sv3d` but cannot create them, so this appends both to the
`avc1` sample entry and grows every enclosing box. The `moov` must follow `mdat`, as ffmpeg writes
it without `+faststart`, so no chunk offset moves.

Usage: python -I spherical.py in.mp4 out.mp4
"""

import struct
import sys

CONTAINERS = {b"moov", b"trak", b"mdia", b"minf", b"stbl"}


def box(kind: bytes, body: bytes) -> bytes:
    return struct.pack(">I", 8 + len(body)) + kind + body


def full(kind: bytes, body: bytes) -> bytes:
    return box(kind, b"\0\0\0\0" + body)


# Each equirectangular bound is the 0.32 fixed-point fraction of the sphere left uncovered: a
# quarter off each side leaves 180 degrees of longitude.
QUARTER = 1 << 30
POSE = full(b"prhd", struct.pack(">iii", 0, 0, 0))
EQUI = full(b"equi", struct.pack(">IIII", 0, 0, QUARTER, QUARTER))
# Stereo mode 2 is left-right.
SPATIAL = full(b"st3d", bytes([2])) + box(b"sv3d", full(b"svhd", b"moq\0") + box(b"proj", POSE + EQUI))


def patch(data: bytearray, start: int, end: int) -> int:
    """Append SPATIAL to the avc1 entry within [start, end), returning how much the range grew."""
    at = start
    while at < end:
        size, kind = struct.unpack(">I4s", data[at : at + 8])
        if size < 8:
            raise ValueError(f"unsupported box size {size} for {kind!r}")
        grown = 0
        if kind in CONTAINERS:
            grown = patch(data, at + 8, at + size)
        elif kind == b"stsd":
            # A full box and an entry count precede the sample entries.
            grown = patch(data, at + 16, at + size)
        elif kind == b"avc1":
            data[at + size : at + size] = SPATIAL
            grown = len(SPATIAL)
        if grown:
            struct.pack_into(">I", data, at, size + grown)
            return grown
        at += size
    return 0


def main() -> None:
    source, target = sys.argv[1:]
    with open(source, "rb") as file:
        data = bytearray(file.read())
    kinds, at = [], 0
    while at < len(data):
        size, kind = struct.unpack(">I4s", data[at : at + 8])
        if size < 8:
            raise ValueError(f"unsupported box size {size} for {kind!r}")
        kinds.append(kind)
        at += size
    # The mdat must precede the moov, so growing the moov moves no sample.
    if kinds.index(b"moov") < kinds.index(b"mdat"):
        raise ValueError("moov precedes mdat; write the source without +faststart")
    if not patch(data, 0, len(data)):
        raise ValueError("no avc1 sample entry")
    with open(target, "wb") as file:
        file.write(data)


main()
