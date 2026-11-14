"""Retime the demo's source, then encode and verify its pre-encoded SD rendition."""

import argparse
import json
import os
import subprocess
from fractions import Fraction
from pathlib import Path


def probe(path):
    return json.loads(
        subprocess.check_output(
            [
                "ffprobe",
                "-v",
                "error",
                "-show_streams",
                "-show_packets",
                "-show_entries",
                "stream=index,codec_type,codec_name,width,height,time_base,duration_ts:packet=stream_index,pts,dts,duration,flags",
                "-of",
                "json",
                str(path),
            ]
        )
    )


def packets(media, stream):
    return [p for p in media["packets"] if p["stream_index"] == stream["index"]]


def video(media):
    stream = next(s for s in media["streams"] if s["codec_type"] == "video")
    return stream, packets(media, stream)


def strict(path, media):
    # A muxer nudges a repeated timestamp by a tick, onto a clock too coarse for another muxer
    # to tell apart, and a demuxer marks the repeat discard.
    for stream in media["streams"]:
        stream_packets = packets(media, stream)
        assert all("D" not in p["flags"] for p in stream_packets), f"{path} stream {stream['index']} discards packets"
        assert all(a["pts"] < b["pts"] for a, b in zip(stream_packets, stream_packets[1:])), (
            f"{path} stream {stream['index']} timestamps do not strictly increase"
        )


def retime(source, target):
    """Give each packet that repeats its predecessor's timestamp a slot of its own.

    The first of the pair takes the frame slot missing just before it, if there is one;
    otherwise the second moves halfway to the next packet, or to its stream's end.
    """
    media = probe(source)
    moves = {}
    for stream in media["streams"]:
        stream_packets = packets(media, stream)
        assert all(p["pts"] == p["dts"] for p in stream_packets), "Retiming assumes no reordering"
        pts = [p["pts"] for p in stream_packets]
        moved = moves[stream["index"]] = {}
        for index in range(1, len(pts)):
            if pts[index] != pts[index - 1]:
                continue
            duration = stream_packets[index - 1]["duration"]
            if index >= 2 and pts[index - 1] - pts[index - 2] == 2 * duration and index - 1 not in moved:
                moved[index - 1] = (pts[index - 1] - duration, duration)
            else:
                end = pts[index + 1] if index + 1 < len(pts) else pts[index] + stream_packets[index]["duration"]
                assert end - pts[index] >= 2, "No room to retime a repeated timestamp"
                middle = pts[index] + (end - pts[index]) // 2
                moved[index] = (middle, end - middle)
        for index, (value, _) in moved.items():
            print(f"stream {stream['index']} packet {index}: {pts[index]} -> {value} ({stream['time_base']})")
    if not any(moves.values()):
        print("Timestamps already strictly increase")
        return

    filters = []
    for index, moved in moves.items():
        ts, duration = "PTS", "DURATION"
        for packet, (value, length) in sorted(moved.items(), reverse=True):
            ts = f"if(eq(N\\,{packet})\\,{value}\\,{ts})"
            duration = f"if(eq(N\\,{packet})\\,{length}\\,{duration})"
        filters += [f"-bsf:{index}", f"setts=ts={ts}:duration={duration}"]
    partial = target.with_suffix(".tmp.mp4")
    subprocess.run(
        [
            "ffmpeg",
            "-hide_banner",
            "-loglevel",
            "warning",
            "-nostdin",
            "-y",
            "-copyts",
            "-i",
            str(source),
            "-map",
            "0",
            "-c",
            "copy",
            *filters,
            "-f",
            "mp4",
            "-movflags",
            "cmaf+separate_moof+delay_moov+skip_trailer",
            "-frag_duration",
            "1000",
            str(partial),
        ],
        check=True,
    )
    retimed = probe(partial)
    strict(partial, retimed)
    for stream in media["streams"]:
        before = [p["pts"] for p in packets(media, stream)]
        after = [p["pts"] for p in packets(retimed, stream)]
        changed = {index: b for index, (a, b) in enumerate(zip(before, after)) if a != b}
        expected = {index: value for index, (value, _) in moves[stream["index"]].items()}
        assert len(before) == len(after) and changed == expected, "Retiming moved other packets"
    assert [s["duration_ts"] for s in retimed["streams"]] == [s["duration_ts"] for s in media["streams"]], (
        "Retiming changed a stream's duration"
    )
    os.replace(partial, target)


def check(source, target):
    source_media = probe(source)
    source_stream, source_packets = video(source_media)
    target_media = probe(target)
    target_stream, target_packets = video(target_media)
    assert len(target_media["streams"]) == 1, "SD must be video only"
    assert (target_stream["codec_name"], target_stream["width"], target_stream["height"]) == ("h264", 640, 360)
    strict(source, source_media)
    strict(target, target_media)
    source_frames = [(p["pts"] * Fraction(source_stream["time_base"]), "K" in p["flags"]) for p in source_packets]
    target_frames = [(p["pts"] * Fraction(target_stream["time_base"]), "K" in p["flags"]) for p in target_packets]
    assert source_frames == target_frames, "SD frame timestamps/keyframes differ from source"

    # Map audio too: it determines the source's loop period. A video-only probe
    # would miss the 39 ms/loop drift caused by the longer audio track.
    output = subprocess.check_output(
        [
            "ffmpeg",
            "-v",
            "error",
            "-copyts",
            "-stream_loop",
            "2",
            "-i",
            str(source),
            "-stream_loop",
            "2",
            "-i",
            str(target),
            "-map",
            "0",
            "-map",
            "1:v",
            "-c",
            "copy",
            "-f",
            "framecrc",
            "-",
        ],
        text=True,
    )
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
    keyframes = sum(key for _, key in source_frames)
    print(f"Verified {len(source_frames)} frames, {keyframes} keyframes, and three aligned loops")


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
    subprocess.run(
        [
            "ffmpeg",
            "-hide_banner",
            "-loglevel",
            "warning",
            "-nostdin",
            "-n",
            "-copyts",
            "-i",
            str(source),
            "-map",
            "0:v:0",
            "-an",
            "-vf",
            "scale=-2:360",
            "-fps_mode",
            "passthrough",
            "-enc_time_base",
            "demux",
            "-c:v",
            "libx264",
            "-preset",
            "slow",
            "-b:v",
            "600k",
            "-maxrate",
            "600k",
            "-bufsize",
            "1200k",
            "-bf",
            "0",
            "-g",
            "2147483647",
            "-sc_threshold",
            "0",
            "-force_key_frames",
            "source",
            "-bsf:v",
            f"setts=duration=if(eq(N\\,{len(packets) - 1})\\,{final_duration}\\,DURATION)",
            "-f",
            "mp4",
            "-movflags",
            "cmaf+separate_moof+delay_moov+skip_trailer",
            "-frag_duration",
            "1000",
            str(target),
        ],
        check=True,
    )
    check(source, target)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=["retime", "encode", "check"])
    args = parser.parse_args()
    source = Path("media/bbb.mp4")
    target = Path("media/bbb-sd.mp4")
    if args.action == "retime":
        retime(source, source)
    elif args.action == "encode":
        encode(source, target)
    else:
        check(source, target)
