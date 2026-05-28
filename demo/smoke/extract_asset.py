#!/usr/bin/env python3
"""Split a raw H.264 Annex-B stream into a language-agnostic asset manifest.

Reads `clip.h264`, writes `asset.json`, `init.bin` (SPS + PPS), and one
`frames/NNNN.bin` per access unit. Native interop clients just forward these
blobs via `publish_media("avc3", init)` / `write_frame(payload, ts_us)`; no
client needs to understand H.264.

Usage: extract_asset.py <clip.h264> <out_dir> [fps]
"""

import json
import sys
from pathlib import Path

START_CODE = b"\x00\x00\x01"

# H.264 NAL unit types (spec table 7-1), masked from the header byte.
NAL_TYPE_MASK = 0x1F
NAL_NON_IDR_SLICE = 1
NAL_IDR_SLICE = 5
NAL_SEI = 6
NAL_SPS = 7
NAL_PPS = 8
NAL_AUD = 9

MICROSECONDS_PER_SECOND = 1_000_000


def nal_units(data: bytes) -> list[bytes]:
    """Return NAL units (without start codes, trailing zeros stripped)."""
    starts = []
    idx = data.find(START_CODE, 0)
    while idx != -1:
        starts.append(idx)
        idx = data.find(START_CODE, idx + 3)

    units = []
    for i, pos in enumerate(starts):
        begin = pos + 3
        end = starts[i + 1] if i + 1 < len(starts) else len(data)
        nal = data[begin:end]
        while nal.endswith(b"\x00"):
            nal = nal[:-1]
        if nal:
            units.append(nal)
    return units


def annexb(nal: bytes) -> bytes:
    """Prepend the 4-byte Annex-B start code (00 00 00 01) to a raw NAL unit."""
    return b"\x00\x00\x00\x01" + nal


def main() -> None:
    if len(sys.argv) < 3:
        print(__doc__, file=sys.stderr)
        sys.exit(2)

    clip = Path(sys.argv[1])
    out = Path(sys.argv[2])
    fps = int(sys.argv[3]) if len(sys.argv) > 3 else 30

    frames_dir = out / "frames"
    frames_dir.mkdir(parents=True, exist_ok=True)

    units = nal_units(clip.read_bytes())

    sps = pps = None
    pending: list[bytes] = []  # non-VCL NALs (SEI/AUD) preceding the next slice
    frames: list[dict] = []
    frame_us = MICROSECONDS_PER_SECOND // fps

    def nal_type(nal: bytes) -> int:
        return nal[0] & NAL_TYPE_MASK

    for nal in units:
        t = nal_type(nal)
        if t == NAL_SPS and sps is None:
            sps = nal
        elif t == NAL_PPS and pps is None:
            pps = nal
        elif t in (NAL_NON_IDR_SLICE, NAL_IDR_SLICE):  # VCL slice -> one access unit / frame
            payload = b"".join(annexb(n) for n in pending) + annexb(nal)
            pending = []
            idx = len(frames)
            path = frames_dir / f"{idx:04d}.bin"
            path.write_bytes(payload)
            frames.append(
                {
                    "file": f"frames/{idx:04d}.bin",
                    "ts_us": idx * frame_us,
                    "keyframe": t == NAL_IDR_SLICE,
                }
            )
        elif t in (NAL_SEI, NAL_AUD):  # non-VCL: carry into the next access unit
            pending.append(nal)
        # SPS/PPS after the first are dropped: they live in init.bin.

    if sps is None or pps is None:
        print("error: no SPS/PPS found in stream", file=sys.stderr)
        sys.exit(1)
    if not frames:
        print("error: no video frames found in stream", file=sys.stderr)
        sys.exit(1)

    (out / "init.bin").write_bytes(annexb(sps) + annexb(pps))
    manifest = {
        "format": "avc3",
        "init_file": "init.bin",
        "fps": fps,
        "frames": frames,
    }
    (out / "asset.json").write_text(json.dumps(manifest, indent=2))
    print(f"wrote {len(frames)} frames to {out}")


if __name__ == "__main__":
    main()
