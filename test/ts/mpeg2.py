#!/usr/bin/env python3
"""Grade a round trip of MPEG-2 video, which the importer carries as whole PES, by decoding it.

    ./mpeg2.py source.ts capture.ts

The importer does not decode MPEG-2 video: it carries each PES whole and the exporter writes it
back unchanged but for its PTS and DTS. So FFmpeg must read the capture's video as MPEG-2, decode
it without an error, and produce the source's pictures: once its first whole GOP starts, every
decoded picture is the source's next one, through the source's last. The subscriber joins at the
relay's latest group, so the run may start up to holes.JOIN into the source, and the pictures of
the GOP it joins in, whose references never arrived, may decode to garbage or not at all.

Exit status is 0 when every check passes, 1 otherwise.
"""

import argparse
import json
import subprocess
import sys
from fractions import Fraction
from pathlib import Path

import holes


def codecs(path):
    """The codec of each audio and video stream FFmpeg finds."""
    report = json.loads(
        subprocess.run(
            ["ffprobe", "-v", "error", "-show_entries", "stream=codec_type,codec_name", "-of", "json", str(path)],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
    )
    return sorted(
        (s["codec_type"], s["codec_name"]) for s in report["streams"] if s["codec_type"] in ("audio", "video")
    )


def pictures(path):
    """The MD5 of each decoded picture, and FFmpeg's decode errors."""
    result = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", str(path), "-map", "0:v:0", "-f", "framemd5", "-"],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode:
        raise SystemExit(f"error: ffmpeg could not decode {path}: {result.stderr.strip()}")
    digests = [
        line.rsplit(",", 1)[1].strip() for line in result.stdout.splitlines() if line and not line.startswith("#")
    ]
    return digests, [line for line in result.stderr.splitlines() if line.strip()]


def grade(source, capture, gop, rate):
    """Raise SystemExit naming the first miss; return a summary otherwise."""
    if codecs(capture) != codecs(source):
        raise SystemExit(f"error: the capture's streams are {codecs(capture)}, the source's {codecs(source)}")
    ours, errors = pictures(source)
    if errors:
        raise SystemExit(f"error: the source does not decode cleanly: {errors[:3]}")
    theirs, errors = pictures(capture)
    known = set(ours)
    # The pictures of the joined GOP before its next I picture may not be the source's.
    lead = next((j for j, digest in enumerate(theirs) if digest in known), None)
    if lead is None:
        raise SystemExit("error: no decoded picture of the capture is the source's")
    if lead > gop:
        raise SystemExit(f"error: the first {lead} pictures are not the source's, past one {gop}-picture GOP")
    run = theirs[lead:]
    # A picture can repeat in the source, so align the run where it matches, preferring the
    # alignment that ends on the source's last picture.
    starts = [k for k, digest in enumerate(ours) if digest == run[0]]
    matches = [k for k in starts if ours[k : k + len(run)] == run]
    first = next((k for k in matches if k + len(run) == len(ours)), matches[0] if matches else starts[0])
    if run != ours[first : first + len(run)]:
        miss = next(j for j, digest in enumerate(run) if first + j >= len(ours) or ours[first + j] != digest)
        raise SystemExit(f"error: decoded picture {lead + miss} is not the source's picture {first + miss}")
    if first + len(run) != len(ours):
        raise SystemExit(f"error: the capture ends at picture {first + len(run)} of the source's {len(ours)}")
    if Fraction(first, rate) > holes.JOIN:
        raise SystemExit(f"error: the capture starts at picture {first}, past the {float(holes.JOIN)}s join")
    # Damage past the join already shows as a picture that is not the source's, so an error is
    # only allowed for missing references at the join.
    if errors and not lead:
        raise SystemExit(f"error: FFmpeg reports decode errors on a capture that joined on a whole GOP: {errors[:3]}")
    return {"pictures": len(run), "source": len(ours), "first": first, "joined": lead}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("source", type=Path)
    parser.add_argument("capture", type=Path)
    parser.add_argument("--gop", type=int, default=25, help="pictures per GOP in the source")
    parser.add_argument("--rate", type=int, default=25, help="pictures per second in the source")
    args = parser.parse_args()
    report = grade(args.source, args.capture, args.gop, args.rate)
    print(
        f"MPEG-2 video: {report['pictures']} of {report['source']} pictures decoded as the source's, "
        f"from picture {report['first']} ({report['joined']} joined pictures skipped)"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
