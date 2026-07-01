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
    docker run -i moqdev/moq-cli import --client-connect https://relay.example.com/anon --broadcast my-stream.hang stdin ts
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
moq-cli <import|export>  <MoQ side>  <endpoint> [endpoint options]
```

- **`import`** routes media INTO MoQ (a source fills the Origin); **`export`**
  routes it OUT (a sink drains the Origin). The verb fixes the data direction.
- **MoQ side** attaches the Origin to the network. At least one of:
  - `--client-connect <url>` dials a relay. The URL path is the relay auth path
    (e.g. `/anon`), `?jwt=<token>` supplies a token, and `--broadcast` names the
    broadcast.
  - `--server-bind <addr>` hosts MoQ sessions directly (with `--tls-generate` /
    `--tls-cert` + `--tls-key`).

  Both may be given at once (dial a relay *and* accept incoming sessions).
- **endpoint** is exactly one of `stdin`/`stdout`, `hls`, `rtmp`, `srt`, `rtc`.
  For the bidirectional gateways, `--connect` dials out and `--listen` binds a
  socket; the parent verb decides whether that pushes or pulls.

Run `moq-cli import --help` / `moq-cli export --help` to see the endpoints, and
`moq-cli import rtmp --help` for a specific one.

## Basic Usage

`moq-cli import ... stdin <format>` reads media from stdin;
`moq-cli export ... stdout <format>` writes it out.

### Publish a Video File

Remux a file to MPEG-TS and pipe it in (`-c copy` avoids re-encoding):

```bash
ffmpeg -i video.mp4 -c copy -f mpegts - | \
    moq-cli import --client-connect https://relay.example.com/anon --broadcast my-stream.hang stdin ts
```

### Capture a Webcam

Pipe an external FFmpeg process as MPEG-TS:

```bash
# macOS
ffmpeg -f avfoundation -i "0:0" -f mpegts - | \
    moq-cli import --client-connect https://relay.example.com/anon --broadcast webcam.hang stdin ts

# Linux
ffmpeg -f v4l2 -i /dev/video0 -f mpegts - | \
    moq-cli import --client-connect https://relay.example.com/anon --broadcast webcam.hang stdin ts
```

### Play a Broadcast

Pull a broadcast back out and play it:

```bash
moq-cli export --client-connect https://relay.example.com/anon --broadcast my-stream.hang stdout fmp4 | ffplay -
```

## Encoding Options

### Low Latency Settings

```bash
ffmpeg -i input.mp4 \
    -c:v libx264 -preset ultrafast -tune zerolatency \
    -g 30 -keyint_min 30 \
    -c:a aac \
    -f mpegts - | moq-cli import --client-connect https://relay.example.com/anon --broadcast my-stream.hang stdin ts
```

## Container Formats

`stdin` (import) and `stdout` (export) select the container as a positional value.

Import (`stdin <format>`):

- `avc3` - raw H.264 Annex-B
- `fmp4` - fragmented MP4 / CMAF
- `ts` - MPEG-TS (H.264 / H.265 video; AAC, MP2, AC-3, or E-AC-3 audio)
- `flv` - FLV / RTMP (H.264 video, AAC audio)

Export (`stdout <format>`):

- `fmp4` - fragmented MP4 / CMAF
- `mkv` - Matroska / WebM
- `ts` - MPEG-TS
- `flv` - FLV / RTMP (H.264 video, AAC audio)

`stdout` also takes `--catalog` to pick which catalog track to read for track
discovery. When omitted, it's auto-detected from the broadcast name suffix
(`.hang` -> `hang`, `.msf` -> `msf`), falling back to `hang`:

- `hang` - the `catalog.json` JSON catalog (default)
- `hangz` - the DEFLATE-compressed `catalog.json.z` catalog (opt-in; shares the `.hang` suffix and is never auto-detected)
- `msf` - the MSF `catalog` track

### MPEG-TS

Ingest an MPEG-TS stream from FFmpeg and play one back out:

```bash
# Import: remux a file to MPEG-TS and pipe it in
ffmpeg -i input.mp4 -c copy -f mpegts - | \
    moq-cli import --client-connect https://relay.example.com --broadcast my-stream.hang stdin ts

# Export: pull MPEG-TS back out and play it
moq-cli export --client-connect https://relay.example.com --broadcast my-stream.hang stdout ts | ffplay -
```

TS export carries H.264 / H.265 as Annex-B and AAC as ADTS. Both in-band
(avc3 / hev1) and out-of-band (avc1 / hvc1, e.g. from an fMP4 import) video
sources work: the parameter sets are read from the bitstream or the catalog
`description` and re-injected as Annex-B on each keyframe.

Broadcast audio (MP2, AC-3, E-AC-3) is carried verbatim: complete, well-formed
frames pass through byte-exact, never transcoded; malformed input is rejected
rather than mis-described. Elementary streams the CLI does not decode (SCTE-35
cues, teletext, DVB subtitles, ...) are carried verbatim too, one MoQ track per
PID, described in the catalog `mpegts` section, and survive `import ... stdin ts`
/ `export ... stdout ts` end-to-end.

### FLV

```bash
# Import: remux a file to FLV and pipe it in
ffmpeg -i input.mp4 -c copy -f flv - | \
    moq-cli import --client-connect https://relay.example.com --broadcast my-stream.hang stdin flv

# Export: pull FLV back out and play it
moq-cli export --client-connect https://relay.example.com --broadcast my-stream.hang stdout flv | ffplay -
```

FLV is the classic RTMP container: H.264 video and AAC audio, each with an
out-of-band header. The enhanced E-RTMP FourCC payloads (HEVC, AV1, Opus) and the
older codecs (VP6, MP3) are not supported on the stdin/stdout path.

## HLS / LL-HLS

Import a remote HLS master/media playlist into a MoQ broadcast:

```bash
moq-cli import --client-connect https://relay.example.com/anon --broadcast my-stream.hang \
    hls https://example.com/live/master.m3u8
```

Serve MoQ broadcasts as HLS / LL-HLS over HTTP:

```bash
moq-cli export --client-connect https://relay.example.com/anon hls --listen '[::]:8089'
```

## Network Gateways (RTMP / SRT / WebRTC)

The `rtmp`, `srt`, and `rtc` endpoints bridge other live protocols. Each takes
either `--connect <url>` (dial out) or `--listen <addr>` (bind a socket), and the
parent verb decides the role:

- **import `--listen`** accepts pushes only (an RTMP/SRT publish, a WHIP publish).
- **export `--listen`** serves plays only (an RTMP/SRT play, a WHEP play).

A listener is directional: an import listener rejects plays, and an export
listener rejects publishes. The operator declares the direction; the connecting
peer can't choose.

### RTMP ingest to a relay

Accept OBS / FFmpeg RTMP pushes and forward them to a relay (broadcasts named
from the RTMP app/key):

```bash
moq-cli import --client-connect https://relay.example.com/anon rtmp --listen '[::]:1935'
```

### Restream MoQ to Twitch (RTMP)

Pull a broadcast from a relay and push it to a remote RTMP server:

```bash
moq-cli export --client-connect https://relay.example.com/anon --broadcast my-stream.hang \
    rtmp --connect 'rtmp://live.twitch.tv/app/<stream-key>'
```

### SRT

```bash
# Accept incoming SRT publishes and forward to a relay
moq-cli import --client-connect https://relay.example.com/anon srt --listen '[::]:9000'

# Serve a broadcast to SRT players
moq-cli export --client-connect https://relay.example.com/anon srt --listen '[::]:9000'
```

### WebRTC (WHIP / WHEP)

Direction picks the HTTP role: import `--listen` is a WHIP server, export
`--listen` is a WHEP server.

```bash
# WHIP ingest: browsers publish to us, we forward to a relay
moq-cli import --client-connect https://relay.example.com/anon rtc --listen '[::]:8080'

# WHEP playback: serve a broadcast to browsers
moq-cli export --client-connect https://relay.example.com/anon rtc --listen '[::]:8080'
```

## Authentication

Pass a JWT token via the URL's `?jwt=` query parameter:

```bash
ffmpeg -i video.mp4 -c copy -f mpegts - | \
    moq-cli import --client-connect "https://relay.example.com/?jwt=<token>" --broadcast my-stream.hang stdin ts
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
    RUST_LOG=debug moq-cli import --client-connect https://relay.example.com/anon --broadcast my-stream.hang stdin ts
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
