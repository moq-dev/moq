#!/usr/bin/env python3
"""Decode and compare a closed-GOP H.264/AAC TS round trip using software FFmpeg."""

import argparse
import json
import subprocess
import sys
from dataclasses import dataclass
from fractions import Fraction
from pathlib import Path

import compliance

TICK = Fraction(1, 90000)


class Failure(Exception):
    """A named assertion failure, also used by the negative controls."""

    def __init__(self, check, detail):
        self.check = check
        super().__init__(detail)


@dataclass
class Frame:
    pts: Fraction
    duration: int
    size: int
    digest: str


def run(command, log):
    """Keep diagnostics even when an external validator fails."""
    with log.open("w") as stderr:
        result = subprocess.run(command, stdout=subprocess.PIPE, stderr=stderr, check=False)
    if result.returncode:
        raise Failure("decode", f"{command[0]} exited {result.returncode}: {log}")
    return result.stdout


def decode(path, output):
    output.mkdir(parents=True, exist_ok=True)
    # A stream may start past FFmpeg's default 5 MB probe window.
    probe = run([
        "ffprobe", "-v", "error", "-probesize", str(path.stat().st_size),
        "-analyzeduration", str(2**62), "-show_streams", "-of", "json", str(path),
    ], output / "probe.log")
    (output / "probe.json").write_bytes(probe)
    streams = json.loads(probe)["streams"]
    media = [s for s in streams if s["codec_type"] in ("video", "audio")]
    video = [s for s in media if s["codec_type"] == "video"]
    audio = [s for s in media if s["codec_type"] == "audio"]
    if len(video) != 1 or len(audio) != 1:
        raise Failure("streams", f"expected one video and one audio stream, got {len(video)} and {len(audio)}: {path}")
    video, audio = video[0], audio[0]
    if video["codec_name"] != "h264" or audio["codec_name"] != "aac" or audio.get("profile") != "LC":
        raise Failure("profile", f"only the closed-GOP H.264/AAC-LC profile is covered: {path}")

    # Disable timestamp repair, frame duplication, hardware decode and tolerant
    # error handling. PCM is also retained so comparison is sample-aligned, not
    # tied to the framehash muxer's audio packet boundaries.
    command = [
        "ffmpeg",
        "-nostdin",
        "-y",
        "-hide_banner",
        "-v",
        "error",
        "-xerror",
        "-err_detect",
        "explode",
        "-abort_on",
        "empty_output+empty_output_stream",
        "-copyts",
        "-hwaccel",
        "none",
        "-threads",
        "1",
        "-i",
        str(path),
        "-map",
        f"0:{video['index']}",
        "-map",
        f"0:{audio['index']}",
        "-c:v",
        "rawvideo",
        "-threads:v",
        "1",
        "-fps_mode",
        "passthrough",
        "-enc_time_base:v",
        "1:90000",
        "-c:a",
        "pcm_s16le",
        "-f",
        "framehash",
        str(output / "frames.sha256"),
        "-map",
        f"0:{audio['index']}",
        "-c:a",
        "pcm_s16le",
        "-f",
        "s16le",
        str(output / "audio.pcm"),
        "-map",
        f"0:{audio['index']}",
        "-c:a",
        "copy",
        "-f",
        "adts",
        str(output / "audio.aac"),
    ]
    (output / "command.json").write_text(json.dumps(command, indent=2) + "\n")
    run(command, output / "decode.log")
    # Some codec errors are logged without affecting the process exit status.
    if (output / "decode.log").stat().st_size:
        raise Failure("decode", f"FFmpeg logged errors: {output / 'decode.log'}")
    frames = [[], []]
    timebases = {}
    for line in (output / "frames.sha256").read_text().splitlines():
        if line.startswith("#tb "):
            index, value = line[4:].split(": ")
            timebases[int(index)] = Fraction(value)
        elif line and not line.startswith("#"):
            index, _dts, pts, duration, size, digest = [x.strip() for x in line.split(",")]
            index = int(index)
            frames[index].append(Frame(int(pts) * timebases[index], int(duration), int(size), digest))
    if not all(frames):
        raise Failure("decode", f"a declared stream decoded no frames: {path}")
    return video, audio, frames, (output / "audio.pcm").read_bytes()


def audio_reference(output, samples, rate):
    """Decode source audio with the same cold-start state as the subscriber."""
    data = (output / "source/audio.aac").read_bytes()
    pos = count = 0
    while count < samples:
        try:
            size, duration = compliance.adts_frame(data, pos)
        except compliance.Refused as error:
            raise Failure("profile", str(error)) from error
        if size < 7 or pos + size > len(data):
            raise Failure("profile", "truncated source ADTS frame")
        pos += size
        count += round(duration * rate)
    if count != samples:
        raise Failure("audio-timing", "admitted audio does not start at a source AAC access-unit boundary")
    reference = output / "reference.aac"
    reference.write_bytes(data[pos:])
    log = output / "reference.log"
    pcm = run(
        [
            "ffmpeg",
            "-nostdin",
            "-hide_banner",
            "-v",
            "error",
            "-xerror",
            "-err_detect",
            "explode",
            "-threads",
            "1",
            "-i",
            str(reference),
            "-map",
            "0:a:0",
            "-c:a",
            "pcm_s16le",
            "-f",
            "s16le",
            "pipe:1",
        ],
        log,
    )
    if log.stat().st_size:
        raise Failure("decode", f"reference AAC decoder logged errors: {log}")
    return pcm


def compare(source, capture, output):
    """Compare the captured suffix, including its tail, with one shared A/V rebase."""
    sv, sa, sf, spcm = decode(source, output / "source")
    cv, ca, cf, cpcm = decode(capture, output / "capture")
    for keys, expected, actual in [
        (("width", "height", "pix_fmt"), sv, cv),
        (("sample_rate", "channels", "channel_layout"), sa, ca),
    ]:
        if any(expected.get(k) != actual.get(k) for k in keys):
            raise Failure("streams", f"stream configuration changed: {keys}")

    # Startup may admit a suffix. EOF is not a licence to lose the last frames:
    # matching backwards pins the tail and detects missing/duplicated pictures.
    start = len(sf[0]) - len(cf[0])
    if start < 0 or [f.digest for f in sf[0][start:]] != [f.digest for f in cf[0]]:
        raise Failure("video-content", "decoded pictures are not an intact suffix of the source")
    offset = cf[0][0].pts - sf[0][start].pts
    if any(abs(c.pts - s.pts - offset) > TICK for s, c in zip(sf[0][start:], cf[0])):
        raise Failure("video-timing", "video timing changed within the admitted interval")

    rate = int(sa["sample_rate"])
    stride = 2 * sa["channels"]
    for label, rows, pcm in [("source", sf[1], spcm), ("capture", cf[1], cpcm)]:
        samples = 0
        for frame in rows:
            # PTS is quantized at 90 kHz, then FFmpeg rounds it onto the
            # sample clock. Relative timestamps can differ by one sample plus
            # one TS tick even when every AAC access unit is contiguous.
            if abs(frame.pts - rows[0].pts - Fraction(samples, rate)) > TICK + Fraction(1, rate):
                raise Failure("audio-timing", f"{label} has a gap or overlap in decoded audio")
            if frame.size % stride:
                raise Failure("audio-content", f"{label} has an incomplete PCM sample")
            samples += frame.size // stride
        if not samples or samples * stride != len(pcm):
            raise Failure("audio-content", f"{label} PCM length disagrees with decoded frame sizes")

    audio_start = (cf[1][0].pts - offset - sf[1][0].pts) * rate
    # PTS quantization and FFmpeg's sample timebase can differ by one sample.
    # Content, length and the common video rebase must all agree on that offset.
    candidates = [
        n
        for n in range(round(audio_start) - 1, round(audio_start) + 2)
        if n >= 0 and abs(Fraction(n, rate) - audio_start / rate) <= TICK + Fraction(1, rate)
    ]
    candidates = [n for n in candidates if len(spcm) - n * stride == len(cpcm)]
    if not candidates:
        raise Failure("audio-length", "audio does not reach the source tail at the shared A/V timestamp offset")
    # Length pins a unique sample offset. AAC's MDCT overlap and perceptual
    # noise state depend on earlier frames, so replay the admitted source AUs
    # from a cold decoder too. No arbitrary warmup samples are excluded.
    audio_start = candidates[0]
    reference = audio_reference(output, audio_start, rate) if audio_start else spcm
    if cpcm != reference:
        raise Failure("audio-content", "aligned decoded PCM samples differ from the source")
    return {
        "profile": "closed-gop-h264-aac-lc",
        "video_frames": len(cf[0]),
        "source_video_frames": len(sf[0]),
        "startup_video_frames": start,
        "audio_samples": len(cpcm) // stride,
        "startup_audio_samples": audio_start,
        "cold_audio_reference": bool(audio_start),
        "compared_audio_samples": len(cpcm) // stride,
        "timestamp_offset_seconds": float(offset),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("capture", type=Path)
    parser.add_argument("--output", required=True, type=Path, help="directory for decoder evidence and report.json")
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    report = {"source": str(args.source), "capture": str(args.capture)}
    try:
        report.update(compare(args.source, args.capture, args.output))
        report["status"] = "PASS"
    except Failure as error:
        report.update(status="FAIL", check=error.check, detail=str(error))
    (args.output / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report, indent=2))
    return int(report["status"] != "PASS")


if __name__ == "__main__":
    sys.exit(main())
