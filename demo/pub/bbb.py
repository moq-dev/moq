"""Encode and verify the demo's pre-encoded SD rendition."""

import argparse
import json
import subprocess
from fractions import Fraction
from pathlib import Path


def probe(path):
    return json.loads(subprocess.check_output([
        "ffprobe", "-v", "error", "-show_streams", "-show_packets",
        "-show_entries", "stream=index,codec_type,codec_name,width,height,time_base,duration_ts:packet=stream_index,pts,duration,flags",
        "-of", "json", str(path),
    ]))


def video(media):
    stream = next(s for s in media["streams"] if s["codec_type"] == "video")
    # Fragmented BBB contains duplicate packets marked discard, not visible frames.
    packets = [p for p in media["packets"] if p["stream_index"] == stream["index"] and "D" not in p["flags"]]
    return stream, packets


def check(source, target):
    source_media = probe(source)
    source_stream, source_packets = video(source_media)
    target_media = probe(target)
    target_stream, target_packets = video(target_media)
    assert len(target_media["streams"]) == 1, "SD must be video only"
    assert (target_stream["codec_name"], target_stream["width"], target_stream["height"]) == ("h264", 640, 360)
    for stream, packets in [(source_stream, source_packets), (target_stream, target_packets)]:
        assert all(a["pts"] < b["pts"] for a, b in zip(packets, packets[1:])), "Non-increasing frame timestamps"
    source_frames = [(p["pts"] * Fraction(source_stream["time_base"]), "K" in p["flags"]) for p in source_packets]
    target_frames = [(p["pts"] * Fraction(target_stream["time_base"]), "K" in p["flags"]) for p in target_packets]
    assert source_frames == target_frames, "SD frame timestamps/keyframes differ from source"

    # Map audio too: it determines the source's loop period. A video-only probe
    # would miss the 39 ms/loop drift caused by the longer audio track.
    output = subprocess.check_output([
        "ffmpeg", "-v", "error", "-copyts", "-stream_loop", "2", "-i", str(source),
        "-stream_loop", "2", "-i", str(target), "-map", "0", "-map", "1:v",
        "-c", "copy", "-f", "framecrc", "-",
    ], text=True)
    timestamps = {}
    time_bases = {}
    for line in output.splitlines():
        if line.startswith("#tb "):
            index, base = line[4:].split(": ")
            time_bases[int(index)] = Fraction(base)
        elif not line.startswith("#"):
            fields = line.split(",")
            index, _, pts = map(int, fields[:3])
            timestamps.setdefault(index, set()).add(pts * time_bases[index])
    target_index = len(source_media["streams"])
    assert timestamps[source_stream["index"]] == timestamps[target_index], "Renditions drift across loops"
    print(f"Verified {len(source_frames)} frames, {sum(key for _, key in source_frames)} keyframes, and three aligned loops")


def encode(source, target):
    media = probe(source)
    stream, packets = video(media)
    base = Fraction(stream["time_base"])
    # FFmpeg loops all selected source streams at the longest stream duration.
    # Hold the SD's final frame for the same period, rounded to the video clock.
    duration = max(s["duration_ts"] * Fraction(s["time_base"]) for s in media["streams"])
    loop_ticks = int(duration / base + Fraction(1, 2))
    final_duration = loop_ticks - (packets[-1]["pts"] - packets[0]["pts"])
    assert final_duration > 0, "Source duration ends before its final video frame"
    subprocess.run([
        "ffmpeg", "-hide_banner", "-loglevel", "warning", "-nostdin", "-n", "-copyts", "-i", str(source),
        "-map", "0:v:0", "-an", "-vf", "scale=-2:360", "-fps_mode", "passthrough",
        "-enc_time_base", "demux", "-c:v", "libx264", "-preset", "slow",
        "-b:v", "600k", "-maxrate", "600k", "-bufsize", "1200k", "-bf", "0",
        "-g", "2147483647", "-sc_threshold", "0", "-force_key_frames", "source",
        "-bsf:v", f"setts=duration=if(eq(N\\,{len(packets) - 1})\\,{final_duration}\\,DURATION)",
        "-f", "mp4", "-movflags", "cmaf+separate_moof+delay_moov+skip_trailer",
        "-frag_duration", "1000", str(target),
    ], check=True)
    check(source, target)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["encode", "check"])
    args = parser.parse_args()
    source = Path("media/bbb.mp4")
    target = Path("media/bbb-sd.mp4")
    if args.action == "encode":
        encode(source, target)
    else:
        check(source, target)
