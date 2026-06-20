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
docker run -v "$(pwd)/video.mp4:/app/video.mp4:ro" moqdev/moq-cli publish /app/video.mp4 https://relay.example.com/anon/stream
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

### Publish a Video File

```bash
moq-cli publish video.mp4 https://relay.example.com/anon/my-stream
```

### Publish from FFmpeg

Pipe FFmpeg output directly to moq-cli:

```bash
ffmpeg -i input.mp4 -f mpegts - | moq-cli publish - https://relay.example.com/anon/my-stream
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
ffmpeg -f avfoundation -i "0:0" -f mpegts - | moq-cli publish - https://relay.example.com/anon/webcam

# Linux
ffmpeg -f v4l2 -i /dev/video0 -f mpegts - | moq-cli publish - https://relay.example.com/anon/webcam
```

### Publish Screen

```bash
# macOS
ffmpeg -f avfoundation -i "1:" -f mpegts - | moq-cli publish - https://relay.example.com/anon/screen

# Linux (X11)
ffmpeg -f x11grab -i :0.0 -f mpegts - | moq-cli publish - https://relay.example.com/anon/screen
```

## Encoding Options

### Custom Video Settings

```bash
ffmpeg -i input.mp4 \
    -c:v libx264 -preset ultrafast -tune zerolatency \
    -b:v 2500k -maxrate 2500k -bufsize 5000k \
    -c:a aac -b:a 128k \
    -f mpegts - | moq-cli publish - https://relay.example.com/anon/stream
```

### Low Latency Settings

```bash
ffmpeg -i input.mp4 \
    -c:v libx264 -preset ultrafast -tune zerolatency \
    -g 30 -keyint_min 30 \
    -c:a aac \
    -f mpegts - | moq-cli publish - https://relay.example.com/anon/stream
```

### H.265/HEVC

```bash
ffmpeg -i input.mp4 \
    -c:v libx265 -preset ultrafast \
    -c:a aac \
    -f mpegts - | moq-cli publish - https://relay.example.com/anon/stream
```

## Container Formats

`publish` selects its input container with a subcommand; `subscribe` selects its
output container with `--format`.

Publish (read from stdin unless noted):

- `avc3` - raw H.264 Annex-B
- `fmp4` - fragmented MP4 / CMAF
- `ts` - MPEG-TS (H.264 / H.265 video; AAC, MP2, AC-3, or E-AC-3 audio)
- `flv` - FLV / RTMP (H.264 video, AAC audio)
- `hls --playlist <url>` - HLS playlist ingest
- `capture` - capture local devices directly (camera H.264 + microphone Opus; requires the `capture` build feature; does not read stdin)

Subscribe (`--format`):

- `fmp4` - fragmented MP4 / CMAF
- `mkv` - Matroska / WebM
- `ts` - MPEG-TS
- `flv` - FLV / RTMP (H.264 video, AAC audio)

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

Pass a JWT token via the URL:

```bash
moq-cli publish video.mp4 "https://relay.example.com/room/123?jwt=<token>"
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
just clock publish https://relay.example.com/anon

# Subscribe to a clock
just clock subscribe https://relay.example.com/anon
```

## Debugging

### Verbose Output

```bash
RUST_LOG=debug moq-cli publish video.mp4 https://relay.example.com/anon/stream
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
