---
title: FFmpeg / moq-cli
description: Command-line tools for MoQ media
---

# FFmpeg / moq-cli

`moq-cli` is a media router: it wires one endpoint onto a shared MoQ Origin. It
moves media into MoQ from a source, or out of MoQ to a sink, bridging stdin/stdout
(via FFmpeg), HLS, RTMP, SRT, and WebRTC.

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
    docker run -i moqdev/moq-cli --client-connect https://relay.example.com/anon --broadcast my-stream.hang import ts
```

Multi-arch images (`linux/amd64` and `linux/arm64`) are published to [Docker Hub](https://hub.docker.com/r/moqdev/moq-cli).

### From Source

```bash
git clone https://github.com/moq-dev/moq
cd moq
cargo build --release --bin moq-cli
```

The binary will be in `target/release/moq-cli`.

## The grammar

```
moq <MoQ side>  <import|export>  <endpoint> [endpoint options]
```

- **MoQ side** attaches the Origin to the network, and comes before the verb. At
  least one of:

  - `--client-connect <url>` dials a relay. The URL path is the relay auth path
    (e.g. `/anon`), `?jwt=<token>` supplies a token, and `--broadcast` names the
    broadcast.
  - `--server-bind <addr>` hosts MoQ sessions directly (with `--tls-generate` /
    `--tls-cert` + `--tls-key`).

  Both may be given at once (dial a relay *and* accept incoming sessions).
- **`import`** routes media INTO MoQ (a source fills the Origin); **`export`**
  routes it OUT (a sink drains the Origin). The verb fixes the data direction.
- **endpoint** is one subcommand: a container format (`avc3`, `fmp4`, `ts`, `flv`
  read from stdin on import; `fmp4`, `mkv`, `ts`, `flv` written to stdout on
  export), or a gateway (`hls`, `rtmp`, `srt`, `rtc`). For the bidirectional
  gateways, `--connect` dials out and `--listen` binds a socket; the parent verb
  decides whether that pushes or pulls.

Run `moq import --help` / `moq export --help` to see the endpoints, and
`moq import rtmp --help` for a specific one.

## Basic Usage

`moq <MoQ side> import <format>` reads a container from stdin;
`moq <MoQ side> export <format>` writes one to stdout.

### Publish a Video File

Remux a file to MPEG-TS and pipe it in (`-c copy` avoids re-encoding):

```bash
ffmpeg -i video.mp4 -c copy -f mpegts - | \
    moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang import ts
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
cargo run -p moq-cli --features capture -- --client-connect https://relay.example.com/anon --broadcast cam.hang import capture

# Default camera + microphone, hardware-encoded H.264 when available:
moq --client-connect https://relay.example.com/anon --broadcast cam.hang import capture

# Pick devices, resolution, and bitrates:
moq --client-connect https://relay.example.com/anon --broadcast cam.hang \
    import capture --camera 0 --width 1280 --height 720 --fps 30 --bitrate 3000000 \
                   --microphone "MacBook Pro Microphone" --audio-bitrate 64000

# One medium only:
moq --client-connect https://relay.example.com/anon --broadcast cam.hang import capture --no-audio
moq --client-connect https://relay.example.com/anon --broadcast cam.hang import capture --no-video

# Pick a codec (default h264). h265 is hardware-only:
moq --client-connect https://relay.example.com/anon --broadcast cam.hang import capture --codec h265
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
ffmpeg -f avfoundation -i "0:0" -f mpegts - | \
    moq --client-connect https://relay.example.com/anon --broadcast webcam.hang import ts

# Linux
ffmpeg -f v4l2 -i /dev/video0 -f mpegts - | \
    moq --client-connect https://relay.example.com/anon --broadcast webcam.hang import ts
```

### Play a Broadcast

Pull a broadcast back out and play it:

```bash
moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang export fmp4 | ffplay -
```

## Encoding Options

### Low Latency Settings

```bash
ffmpeg -i input.mp4 \
    -c:v libx264 -preset ultrafast -tune zerolatency \
    -g 30 -keyint_min 30 \
    -c:a aac \
    -f mpegts - | moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang import ts
```

## Container Formats

The container format is the endpoint subcommand: `import <format>` reads it from
stdin, `export <format>` writes it to stdout.

Import formats:

- `avc3` - raw H.264 Annex-B
- `fmp4` - fragmented MP4 / CMAF
- `ts` - MPEG-TS (H.264 / H.265 video; AAC, MP2, AC-3, or E-AC-3 audio)
- `flv` - FLV / RTMP (H.264 video, AAC audio)
- `capture` - capture local devices directly (camera H.264 + microphone Opus; requires the `capture` build feature; does not read stdin)

Export formats:

- `fmp4` - fragmented MP4 / CMAF
- `mkv` - Matroska / WebM
- `ts` - MPEG-TS
- `flv` - FLV / RTMP (H.264 video, AAC audio)

`export` also takes `--catalog-format` to pick which catalog track to read for track
discovery. When omitted, it's auto-detected from the broadcast name suffix
(`.hang` -> `hang`, `.msf` -> `msf`), falling back to `hang`:

- `hang` - the `catalog.json` JSON catalog (default)
- `hangz` - the DEFLATE-compressed `catalog.json.z` catalog (opt-in; shares the `.hang` suffix and is never auto-detected)
- `msf` - the MSF `catalog` track

Every export sink caps how long a stalled group is waited on before the muxer
skips to a newer one. Each owns the knob so its default fits the transport: the
stdout containers and `rtmp` take `--latency-max` (default `500ms`), `hls` takes
`--latency-max` (default `10s`, generous so live GOPs aren't skipped while
segments build), and `srt` reuses its `--latency` (the receive buffer doubles as
the skip threshold). WebRTC (`rtc`) is real-time and doesn't buffer, so it has no
such knob.

### MPEG-TS

Ingest an MPEG-TS stream from FFmpeg and play one back out:

```bash
# Import: remux a file to MPEG-TS and pipe it in
ffmpeg -i input.mp4 -c copy -f mpegts - | \
    moq --client-connect https://relay.example.com --broadcast my-stream.hang import ts

# Export: pull MPEG-TS back out and play it
moq --client-connect https://relay.example.com --broadcast my-stream.hang export ts | ffplay -
```

TS export carries H.264 / H.265 as Annex-B and AAC as ADTS. Both in-band
(avc3 / hev1) and out-of-band (avc1 / hvc1, e.g. from an fMP4 import) video
sources work: the parameter sets are read from the bitstream or the catalog
`description` and re-injected as Annex-B on each keyframe.

Broadcast audio (MP2, AC-3, E-AC-3) is carried verbatim: complete, well-formed
frames pass through byte-exact, never transcoded; malformed input is rejected
rather than mis-described. Elementary streams the CLI does not decode (SCTE-35
cues, teletext, DVB subtitles, ...) are carried verbatim too, one MoQ track per
PID, described in the catalog `mpegts` section, and survive `import ts` /
`export ts` end-to-end.

### FLV

```bash
# Import: remux a file to FLV and pipe it in
ffmpeg -i input.mp4 -c copy -f flv - | \
    moq --client-connect https://relay.example.com --broadcast my-stream.hang import flv

# Export: pull FLV back out and play it
moq --client-connect https://relay.example.com --broadcast my-stream.hang export flv | ffplay -
```

FLV is the classic RTMP container: H.264 video and AAC audio, each with an
out-of-band header. The enhanced E-RTMP FourCC payloads (HEVC, AV1, Opus) and the
older codecs (VP6, MP3) are not supported on the stdin/stdout container path.

## HLS / LL-HLS

Import a remote HLS master/media playlist into a MoQ broadcast:

```bash
moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang \
    import hls https://example.com/live/master.m3u8
```

Serve one MoQ broadcast as HLS / LL-HLS over HTTP (reached at
`http://host:8089/<broadcast>/master.m3u8`):

```bash
moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang \
    export hls --listen '[::]:8089'
```

HLS export does not allow cross-origin browser access unless configured. Pass
`--cors-origin <origin>` one or more times to allow specific origins, or
`--cors-origin '*'` to allow any browser origin.

## Network Gateways (RTMP / SRT / WebRTC)

The `rtmp`, `srt`, and `rtc` endpoints bridge other live protocols. Each takes
either `--connect <url>` (dial out) or `--listen <addr>` (bind a socket), and the
parent verb decides the role:

- **import `--listen`** accepts pushes only (an RTMP/SRT publish, a WHIP publish).
- **export `--listen`** serves plays only (an RTMP/SRT play, a WHEP play).

A listener is directional: an import listener rejects plays, and an export
listener rejects publishes. The operator declares the direction; the connecting
peer can't choose.

Every gateway is scoped to the single `--broadcast` (required for a `--listen`):
a listener bridges only that broadcast, ignoring the RTMP app/key and SRT stream
id. (Multi-broadcast routing by app/key belongs behind a relay, via the gateway
libraries' auth-aware API.)

### RTMP ingest to a relay

Accept OBS / FFmpeg RTMP pushes and forward one broadcast to a relay:

```bash
moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang \
    import rtmp --listen '[::]:1935'
```

### Restream MoQ to Twitch (RTMP)

Pull a broadcast from a relay and push it to a remote RTMP server:

```bash
moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang \
    export rtmp --connect 'rtmp://live.twitch.tv/app/<stream-key>'
```

### SRT

```bash
# Accept incoming SRT publishes as one broadcast and forward to a relay
moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang import srt --listen '[::]:9000'

# Serve a broadcast to SRT players
moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang export srt --listen '[::]:9000'
```

### WebRTC (WHIP / WHEP)

Direction picks the HTTP role: import `--listen` is a WHIP server, export
`--listen` is a WHEP server. Peers reach the broadcast at
`http://host:8080/<broadcast>`.

```bash
# WHIP ingest: browsers publish one broadcast to us, we forward to a relay
moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang import rtc --listen '[::]:8080'

# WHEP playback: serve a broadcast to browsers
moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang export rtc --listen '[::]:8080'
```

The WHIP/WHEP HTTP listener does not allow cross-origin browser access unless
configured. Pass `--cors-origin <origin>` one or more times to allow specific
origins, or `--cors-origin '*'` to allow any browser origin.

## Authentication

Pass a JWT token via the URL's `?jwt=` query parameter:

```bash
ffmpeg -i video.mp4 -c copy -f mpegts - | \
    moq --client-connect "https://relay.example.com/?jwt=<token>" --broadcast my-stream.hang import ts
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

## Debugging

### Verbose Output

```bash
ffmpeg -i video.mp4 -c copy -f mpegts - | \
    RUST_LOG=debug moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang import ts
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
