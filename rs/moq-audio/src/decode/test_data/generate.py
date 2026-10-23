"""Regenerate encoded vectors and independent FFmpeg-native PCM references.

Usage: nix develop --command python3 rs/moq-audio/src/decode/test_data/generate.py SOURCE.wav
See README.md for the source recording, license and pinned tool version.
"""

import hashlib
from pathlib import Path
import subprocess
import sys

source = Path(sys.argv[1])
expected = "9c2146638c3acba0c05643d18ee3a03fd2d5c167f89cf160579f97266b368cd2"
if hashlib.sha256(source.read_bytes()).hexdigest() != expected:
    raise SystemExit("source recording does not match README.md SHA-256")
out = Path(__file__).resolve().parent


def ff(*args):
    subprocess.run(
        ["ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin", "-y", *map(str, args)],
        check=True,
    )


# Each channel carries a distinct stepped frequency.
for rate in [44100, 48000]:
    expr = "|".join(
        f"0.4*sin(2*PI*({frequency}+if(gte(t,0.2),{frequency},0))*t)"
        for frequency in [400, 1000]
    )
    name = f"aac-{rate}-stereo"
    ff(
        "-f", "lavfi", "-i", f"aevalsrc='{expr}':s={rate}:d=0.4",
        "-c:a", "aac", "-b:a", "128k",
        "-movflags", "empty_moov+frag_every_frame+default_base_moof", out / f"{name}.mp4",
    )
    ff("-c:a", "aac", "-i", out / f"{name}.mp4", "-f", "f32le", out / f"{name}.f32")

# The supported TS path carries family-one 5.1 mapping; CMAF ingest only accepts family zero.
expr = "|".join(f"0.3*sin(2*PI*{frequency}*t)" for frequency in [400, 700, 1000, 80, 1300, 1600])
ff(
    "-f", "lavfi", "-i", f"aevalsrc={expr}:s=48000:d=0.4:c=5.1",
    "-c:a", "libopus", "-b:a", "256k", "-mapping_family", "1",
    "-f", "mpegts", out / "opus-surround.ts",
)
ff("-c:a", "opus", "-i", out / "opus-surround.ts", "-f", "f32le", out / "opus-surround.f32")

for codec, rate, encoder in [("aac", 44100, "aac"), ("opus", 48000, "libopus")]:
    name = f"{codec}-speech"
    ff(
        "-i", source, "-ss", "10", "-t", "0.4", "-ac", "1", "-ar", rate,
        "-c:a", encoder, "-b:a", "64k",
        "-movflags", "empty_moov+frag_every_frame+default_base_moof", out / f"{name}.mp4",
    )
    # Input-side codec selection forces the native decoder, not libopus or Symphonia.
    ff("-c:a", codec, "-i", out / f"{name}.mp4", "-f", "f32le", out / f"{name}.f32")
