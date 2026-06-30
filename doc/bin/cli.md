---
title: FFmpeg / moq-cli
description: Command-line tools for MoQ media
---

# FFmpeg / moq-cli

`moq-cli` is a command-line tool for publishing media to MoQ relays. It works with FFmpeg for encoding.

## Installation

### Using Cargo

```bash
cargo install moq-cli
```

### Using winget (Windows)

```powershell
winget install moq-dev.moq-cli
```

### Using Nix

```bash
# Run directly
nix run github:moq-dev/moq#moq-cli

# Or build and find the binary in ./result/bin/
nix build github:moq-dev/moq#moq-cli
```

### Using Docker

```bash
docker pull moqdev/moq-cli

# moq-cli reads media from stdin, so pipe an MPEG-TS stream into the container.
# `-i` forwards stdin to the container process.
ffmpeg -i video.mp4 -c copy -f mpegts - | \
    docker run -i moqdev/moq-cli publish --url https://relay.example.com/anon --broadcast my-stream ts
```

Multi-arch images (`linux/amd64` and `linux/arm64`) are published to [Docker Hub](https://hub.docker.com/r/moqdev/moq-cli).

### From Source

```bash
git clone https://github.com/moq-dev/moq
cd moq
cargo build --release --bin moq-cli
```

The binary will be in `target/release/moq-cli`.

## Basic Usage

`moq-cli publish` reads media from stdin and selects the input container with a
subcommand (`ts`, `fmp4`, `flv`, `avc3`, `hls`). The destination is set with
`--url` (the server) and `--broadcast` (the broadcast name), not a path on the URL.

### Publish a Video File

Remux a file to MPEG-TS and pipe it in (`-c copy` avoids re-encoding):

```bash
ffmpeg -i video.mp4 -c copy -f mpegts - | \
    moq-cli publish --url https://relay.example.com/anon --broadcast my-stream ts
```

### Publish from FFmpeg

Pipe FFmpeg output directly to moq-cli:

```bash
ffmpeg -i input.mp4 -f mpegts - | moq-cli publish --url https://relay.example.com/anon --broadcast my-stream ts
```

### Capture a Webcam

The `capture` subcommand captures and encodes from local devices directly, no
external FFmpeg process required. It publishes the camera as an H.264 video
track and the microphone as an Opus audio track on the same broadcast. It is
gated behind the `capture` feature:

Build (or run) with the feature enabled:

```bash
cargo build --release -p moq-cli --features capture
# or run straight from a checkout:
cargo run -p moq-cli --features capture -- publish --url https://relay.example.com --broadcast cam.hang capture

# Default camera + microphone, hardware-encoded H.264 when available:
moq-cli publish --url https://relay.example.com --broadcast cam.hang capture

# Pick devices, resolution, and bitrates:
moq-cli publish --url https://relay.example.com --broadcast cam.hang \
    capture --camera 0 --width 1280 --height 720 --fps 30 --bitrate 3000000 \
            --microphone "MacBook Pro Microphone" --audio-bitrate 64000

# One medium only:
moq-cli publish --url https://relay.example.com --broadcast cam.hang capture --no-audio
moq-cli publish --url https://relay.example.com --broadcast cam.hang capture --no-video

# Pick a codec (default h264). h265 is hardware-only:
moq-cli publish --url https://relay.example.com --broadcast cam.hang capture --codec h265
```

On Linux the NVENC (NVIDIA) and VAAPI (Intel/AMD) encoders are compiled in by
default and link the CUDA / libva system libraries. To build `capture` without
them (software openh264 + V4L2 capture only, no CUDA/libva dependency), drop the
default features:

```bash
cargo build --release -p moq-cli --no-default-features \
    --features "iroh quinn websocket capture"
```

Video capture uses a native per-platform backend (AVFoundation on macOS, V4L2 on
Linux, Media Foundation on Windows). The codec is chosen with `--codec`
(`h264` default, or `h265`). For H.264 it picks a hardware encoder
(VideoToolbox on macOS, NVENC on Linux NVIDIA, VAAPI on Linux Intel/AMD) when one
is present, falling back to the built-in software encoder (openh264); force either
with `--hardware` / `--software`. H.265 is hardware-only (VideoToolbox on macOS,
Media Foundation on Windows). `--camera` takes a bare integer as a device index, otherwise a
device path (Linux) or name (a friendly-name substring on Windows, the
AVFoundation `uniqueID` on macOS). Audio capture uses cpal (CoreAudio / WASAPI /
ALSA) and encodes Opus.

Alternatively, pipe an external FFmpeg process as MPEG-TS:

```bash
# macOS
ffmpeg -f avfoundation -i "0:0" -f mpegts - | moq-cli publish --url https://relay.example.com/anon --broadcast webcam ts

# Linux
ffmpeg -f v4l2 -i /dev/video0 -f mpegts - | moq-cli publish --url https://relay.example.com/anon --broadcast webcam ts
```

### Publish Screen

```bash
# macOS
ffmpeg -f avfoundation -i "1:" -f mpegts - | moq-cli publish --url https://relay.example.com/anon --broadcast screen ts

# Linux (X11)
ffmpeg -f x11grab -i :0.0 -f mpegts - | moq-cli publish --url https://relay.example.com/anon --broadcast screen ts
```

## Encoding Options

### Custom Video Settings

```bash
ffmpeg -i input.mp4 \
    -c:v libx264 -preset ultrafast -tune zerolatency \
    -b:v 2500k -maxrate 2500k -bufsize 5000k \
    -c:a aac -b:a 128k \
    -f mpegts - | moq-cli publish --url https://relay.example.com/anon --broadcast my-stream ts
```

### Low Latency Settings

```bash
ffmpeg -i input.mp4 \
    -c:v libx264 -preset ultrafast -tune zerolatency \
    -g 30 -keyint_min 30 \
    -c:a aac \
    -f mpegts - | moq-cli publish --url https://relay.example.com/anon --broadcast my-stream ts
```

### H.265/HEVC

```bash
ffmpeg -i input.mp4 \
    -c:v libx265 -preset ultrafast \
    -c:a aac \
    -f mpegts - | moq-cli publish --url https://relay.example.com/anon --broadcast my-stream ts
```

## Container Formats

`publish` selects its input container with a subcommand; `subscribe` selects its
output container with `--format`.

Publish (read from stdin unless noted):

- `avc3` - raw H.264 Annex-B
- `fmp4` - fragmented MP4 / CMAF
- `ts` - MPEG-TS (H.264 / H.265 video; AAC, MP2, AC-3, or E-AC-3 audio)
- `flv` - FLV / RTMP (H.264 video, AAC audio)
- `capture` - capture local devices directly (camera H.264 + microphone Opus; requires the `capture` build feature; does not read stdin)

Subscribe (`--format`):

- `fmp4` - fragmented MP4 / CMAF
- `mkv` - Matroska / WebM
- `ts` - MPEG-TS
- `flv` - FLV / RTMP (H.264 video, AAC audio)

`subscribe` also takes `--catalog` to pick which catalog track to read for track
discovery. When omitted, it's auto-detected from the broadcast name suffix
(`.hang` -> `hang`, `.msf` -> `msf`), falling back to `hang`:

- `hang` - the `catalog.json` JSON catalog (default)
- `hangz` - the DEFLATE-compressed `catalog.json.z` catalog (opt-in; shares the `.hang` suffix and is never auto-detected)
- `msf` - the MSF `catalog` track

### HLS / LL-HLS

Import an HLS master/media playlist into a MoQ broadcast:

```bash
moq-cli hls --url https://relay.example.com/anon import \
    --broadcast my-stream.hang \
    --playlist https://example.com/live/master.m3u8
```

Serve MoQ broadcasts as HLS / LL-HLS over HTTP:

```bash
moq-cli hls --url https://relay.example.com/anon export --listen '[::]:8089'
```

### MPEG-TS

Ingest an MPEG-TS stream from FFmpeg and play one back out:

```bash
# Publish: remux a file to MPEG-TS and pipe it in
ffmpeg -i input.mp4 -c copy -f mpegts - | \
    moq-cli publish --url https://relay.example.com --broadcast my-stream ts

# Subscribe: pull MPEG-TS back out and play it
moq-cli subscribe --url https://relay.example.com --broadcast my-stream --format ts | ffplay -
```

TS export carries H.264 / H.265 as Annex-B and AAC as ADTS. Both in-band
(avc3 / hev1) and out-of-band (avc1 / hvc1, e.g. from an fMP4 import) video
sources work: the parameter sets are read from the bitstream or the catalog
`description` and re-injected as Annex-B on each keyframe.

Broadcast audio (MP2, AC-3, E-AC-3) is carried verbatim: complete, well-formed
frames pass through byte-exact, never transcoded; malformed input is rejected
rather than mis-described. The catalog describes the codec honestly so a
subscriber that can decode it (typically TS gear) picks it up; browsers cannot
play these codecs and should skip the rendition.

Elementary streams the CLI does not decode (SCTE-35 cues, teletext, DVB
subtitles, private data, ...) are carried verbatim too, one MoQ track per PID,
described in the catalog `mpegts` section. They survive `publish ts | relay |
subscribe --format ts` end-to-end with their original PIDs, PMT descriptors, and
PES stream\_ids, so a contribution feed keeps its ancillary streams. The relay
forwards them transparently and never parses the payload.

### FLV

Ingest an FLV stream from FFmpeg and play one back out:

```bash
# Publish: remux a file to FLV and pipe it in
ffmpeg -i input.mp4 -c copy -f flv - | \
    moq-cli publish --url https://relay.example.com --broadcast my-stream flv

# Subscribe: pull FLV back out and play it
moq-cli subscribe --url https://relay.example.com --broadcast my-stream --format flv | ffplay -
```

FLV is the classic RTMP container: H.264 video carried as length-prefixed NALU
with an out-of-band avcC, and AAC audio carried raw with an out-of-band
AudioSpecificConfig. Both pass straight through to the catalog `description`. The
enhanced E-RTMP FourCC payloads (HEVC, AV1, Opus) and the older codecs (VP6, MP3)
are not supported.

## Authentication

Pass a JWT token via the URL's `?jwt=` query parameter:

```bash
ffmpeg -i video.mp4 -c copy -f mpegts - | \
    moq-cli publish --url "https://relay.example.com/?jwt=<token>" --broadcast my-stream ts
```

See [Authentication](/bin/relay/auth) for token generation.

## Test Videos

The repository includes helper commands for test content:

```bash
# Publish Big Buck Bunny
just pub bbb https://relay.example.com/anon

# Publish Tears of Steel
just pub tos https://relay.example.com/anon
```

## Clock Synchronization

Publish and subscribe to clock broadcasts for testing:

```bash
# Publish a clock
just pub clock publish https://relay.example.com/anon

# Subscribe to a clock
just pub clock subscribe https://relay.example.com/anon
```

## Debugging

### Verbose Output

```bash
ffmpeg -i video.mp4 -c copy -f mpegts - | \
    RUST_LOG=debug moq-cli publish --url https://relay.example.com/anon --broadcast my-stream ts
```

### Check Connection

```bash
# Verify you can connect to the relay
curl http://relay.example.com:4443/announced/
```

## Common Issues

### "Connection refused"

- Ensure the relay is running
- Check firewall allows UDP traffic
- Verify the URL is correct

### "Invalid certificate"

- The relay needs a valid TLS certificate
- For development, use the fingerprint method
- See [TLS Setup](/bin/relay/#tls-setup)

### "Permission denied"

- Check your JWT token is valid
- Verify the token allows publishing to that path
- See [Authentication](/bin/relay/auth)

## Next Steps

- Deploy a [relay server](/bin/relay/)
- Use [Web Components](/lib/js/env/web) for playback
- Try the [Rust libraries](/lib/rs/) for custom apps
