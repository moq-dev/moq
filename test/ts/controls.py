#!/usr/bin/env python3
"""Run real FFmpeg and TSDuck positive/negative controls without a relay."""

import json
import subprocess
import sys
from pathlib import Path

import accuracy
import compliance
import decode


def command(args, log):
    with log.open("w") as output:
        subprocess.run(args, stdout=output, stderr=subprocess.STDOUT, check=True)


def main(root):
    root.mkdir(parents=True, exist_ok=True)
    source = root / "source.ts"
    ffmpeg = ["ffmpeg", "-nostdin", "-y", "-hide_banner", "-v", "error"]
    command(
        ffmpeg
        + [
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=160x96:rate=25",
            "-f",
            "lavfi",
            "-i",
            "aevalsrc=sin(2*PI*(300*t+70*t*t)):s=48000",
            "-t",
            "3",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-threads",
            "1",
            "-x264-params",
            "keyint=25:min-keyint=25:scenecut=0",
            "-c:a",
            "aac",
            "-f",
            "mpegts",
            "-muxrate",
            "1000000",
            "-pcr_period",
            "20",
            "-pes_payload_size",
            "0",
            str(source),
        ],
        root / "generate.log",
    )
    for tool, flag in [
        ("ffmpeg", "-version"),
        ("ffprobe", "-version"),
        ("tsp", "--version"),
        ("tsanalyze", "--version"),
    ]:
        command([tool, flag], root / f"{tool}-version.txt")

    cases = [("unchanged", source, None)]
    for name, options, expected in [
        (
            "remuxed",
            ["-map", "0:v", "-map", "0:a", "-c", "copy", "-output_ts_offset", "5", "-pes_payload_size", "3000"],
            None,
        ),
        (
            "audio-quantization",
            ["-map", "0:v", "-map", "0:a", "-c", "copy", "-bsf:a", "setts=pts=PTS-mod(N\\,3):dts=DTS-mod(N\\,3)"],
            None,
        ),
        ("missing-audio", ["-map", "0:v", "-c", "copy"], "streams"),
        (
            "different-video",
            ["-map", "0:v", "-map", "0:a", "-vf", "negate", "-c:v", "libx264", "-c:a", "copy"],
            "video-content",
        ),
        (
            "different-audio",
            ["-map", "0:v", "-map", "0:a", "-c:v", "copy", "-af", "atrim=start_sample=1024,volume=0", "-c:a", "aac"],
            "audio-content",
        ),
        ("truncated-tail", ["-map", "0:v", "-map", "0:a", "-c", "copy", "-t", "2"], "video-content"),
        (
            "audio-shift",
            ["-map", "0:v", "-map", "0:a", "-c", "copy", "-bsf:a", "setts=pts=PTS+9000:dts=DTS+9000"],
            "audio-length",
        ),
        (
            "video-rate",
            ["-map", "0:v", "-map", "0:a", "-c", "copy", "-bsf:v", "setts=pts=PTS*1.01:dts=DTS*1.01"],
            "video-timing",
        ),
        (
            "audio-gap",
            [
                "-map",
                "0:v",
                "-map",
                "0:a",
                "-c",
                "copy",
                "-bsf:a",
                "setts=pts=PTS+gte(N\\,60)*9000:dts=DTS+gte(N\\,60)*9000",
            ],
            "audio-timing",
        ),
    ]:
        path = root / f"{name}.ts"
        command(ffmpeg + ["-i", str(source)] + options + ["-f", "mpegts", str(path)], root / f"{name}.log")
        cases.append((name, path, expected))

    joined = root / "joined.ts"
    command(
        ffmpeg + ["-ss", "1", "-i", str(source), "-map", "0:v", "-map", "0:a", "-c", "copy", str(joined)],
        root / "joined.log",
    )
    cases.append(("joined", joined, None))

    # Keep the transport/PES headers and parameter sets, damaging an actual
    # coded slice mid-stream. This must reach the decoder, not just fail probing.
    damaged = bytearray(source.read_bytes())
    changed = False
    for off in range(len(damaged) // 2 // 188 * 188, len(damaged), 188):
        pid = (damaged[off + 1] & 0x1F) << 8 | damaged[off + 2]
        if pid != 0x100 or not damaged[off + 3] & 0x10 or damaged[off + 1] & 0x40:
            continue
        start = off + 4 + (1 + damaged[off + 4] if damaged[off + 3] & 0x20 else 0)
        if start < off + 188:
            damaged[start : off + 188] = b"\xff" * (off + 188 - start)
            changed = True
            break
    assert changed, "fixture has no video continuation payload to corrupt"
    corrupt = root / "corrupt-video.ts"
    corrupt.write_bytes(damaged)
    cases.append(("corrupt-video", corrupt, "decode"))
    empty = root / "empty.ts"
    empty.write_bytes(b"")
    cases.append(("empty", empty, "decode"))

    results = []
    for name, path, expected in cases:
        try:
            report = decode.compare(source, path, root / name)
            actual = None
        except decode.Failure as error:
            actual = error.check
            report = {"check": actual, "detail": str(error)}
        results.append({"case": name, "expected": expected, "actual": actual, "report": report})
        (root / "decode-controls.json").write_text(json.dumps(results, indent=2) + "\n")
        assert actual == expected, f"{name}: expected {expected}, got {actual}: {report}"
        if name == "joined":
            assert report["startup_video_frames"] > 0 and report["cold_audio_reference"], report
        print(f"PASS FFmpeg {name}: {actual or 'source content matches'}", flush=True)

    base = source.read_bytes()
    variants = {"clean": base}
    no_pat = bytearray(base)
    for off in range(0, len(no_pat), 188):
        if (no_pat[off + 1] & 0x1F) == 0 and no_pat[off + 2] == 0:
            no_pat[off : off + 188] = b"\x47\x1f\xff\x10" + b"\xff" * 184
    variants["missing-pat"] = no_pat
    cc = bytearray(base)
    tei = bytearray(base)
    off = len(base) // 2 // 188 * 188
    # Choose a media payload, since null PID continuity is intentionally ignored.
    while ((base[off + 1] & 0x1F) << 8 | base[off + 2]) != 0x100 or not base[off + 3] & 0x10:
        off += 188
    cc[off + 3] ^= 1
    tei[off + 1] |= 0x80
    variants.update(continuity=cc, transport_error=tei)
    for name, data in variants.items():
        path = root / f"structure-{name}.ts"
        path.write_bytes(data)
        analysis = compliance.run_tsanalyze(str(path))
        checks = [
            *compliance.check_pat_pmt(analysis),
            compliance.check_sync(analysis),
            compliance.check_continuity(analysis),
        ]
        failed = {c.name for c in checks if c.status == compliance.Status.FAIL}
        expected = {"missing-pat": "pat", "continuity": "continuity", "transport_error": "sync"}.get(name)
        (root / f"structure-{name}.json").write_text(json.dumps(analysis, indent=2) + "\n")
        assert (expected in failed) if expected else not failed, f"TSDuck {name}: {failed}"
        print(f"PASS TSDuck {name}: {sorted(failed)}", flush=True)

    accuracy.check(source, 1_000_000, 0, root / "pcr-clean.log")
    for name, rate, skip in [("wrong-rate", 2_000_000, 0), ("no-samples", 1_000_000, len(base) // 188)]:
        try:
            accuracy.check(source, rate, skip, root / f"pcr-{name}.log")
        except RuntimeError as error:
            assert "PCR accuracy failed" in str(error), str(error)
        else:
            raise AssertionError(f"pcrverify accepted {name}")
        print(f"PASS TSDuck PCR {name}", flush=True)


if __name__ == "__main__":
    main(Path(sys.argv[1]))
