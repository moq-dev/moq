---
title: moq-cli
description: The moq media router, for publishing, playing, converting, and gatewaying
---

# moq-cli

`moq` is a media router. One process connects to a relay (or hosts sessions
itself) and moves media into MoQ from a source, out of MoQ to a sink, or plays
it locally. Install it with `cargo install moq-cli`, brew, apt, dnf, winget,
or Docker; see [Install](/setup/install).

## What it does

| Verb | Endpoint | |
| --- | --- | --- |
| `import` | `ts`, `fmp4`, `flv`, `avc3` | Read a container from stdin (usually FFmpeg). |
| `import` | `capture` | Capture a camera, display, window, or app plus a microphone, and encode natively. |
| `import` | `hls <url>` | Pull a remote HLS playlist. |
| `import` | `rtmp`, `srt`, `rtc` | Accept pushes (`--listen`) or pull from a remote (`--connect`). |
| `export` | `fmp4`, `mkv`, `ts`, `flv`, `h264`, `h265` | Write a container to stdout. |
| `export` | `hls --listen` | Serve the broadcast as HLS over HTTP. |
| `export` | `rtmp`, `srt`, `rtc` | Serve plays (`--listen`) or push to a remote (`--connect`). |
| `play` | | Decode and play in a native window with sound. |
| `transcode` | | Publish a just-in-time rendition ladder next to a broadcast. |
| `token` | | Generate, sign, and verify relay JWTs. |
| `devices` | | List capture sources and their ids. |

## Grammar

```text
moq <MoQ side> import <source> [options]
moq <MoQ side> export <sink> [options]
moq <MoQ side> play [options]
```

The **MoQ side** goes first and attaches the process to the network:
`--client-connect <url>` dials a relay (the path is the auth path, `?jwt=`
carries a token), and `--broadcast <name>` names the broadcast. A process can
instead host sessions with `--server-bind`, or both at once. `moq import --help` lists the sources and `moq import rtmp --help` a specific one.

```bash
# Publish a file (remux to MPEG-TS without re-encoding)
ffmpeg -re -i video.mp4 -c copy -f mpegts - | \
    moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang import ts

# Pull it back out
moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang export fmp4 | ffplay -

# With a token
moq --client-connect "https://relay.example.com/rooms/1?jwt=$TOKEN" --broadcast alice.hang import ts
```

MPEG-TS import carries H.264/H.265 and AAC/MP2/AC-3/E-AC-3, passes SCTE-35 and
subtitle PIDs through as tracks, and round-trips the service tables. FLV
covers H.264 + AAC.

## Play

```bash
moq --client-connect https://relay.example.com/anon --broadcast my-stream.hang play
```

Decodes H.264, H.265, and AV1 video and Opus, PCM, and AAC-LC audio using
the platform hardware decoder where available. `--video-name` and
`--audio-name` pick a rendition; `--latency-max` (default 500 ms) bounds how
far a stalled group may lag before it is skipped. Each role follows the catalog
for as long as it lasts, so a publisher that retires the rendition being played
ends that track and the role picks a replacement. Playback is behind the
`play` feature, since it pulls in windowing and audio-device dependencies:

```bash
cargo install moq-cli --no-default-features --features "iroh,quinn,websocket,play"
```

## Capture

```bash
moq --client-connect https://relay.example.com/anon --broadcast cam.hang import capture
moq ... import capture --display --system-audio          # share a screen with its sound (macOS)
moq ... import capture --window 39193 --no-audio         # one window (macOS, Windows, X11)
moq ... import capture --camera 0 --width 1280 --height 720 --fps 30 --bitrate 3000000 --codec h265
```

Video goes through the platform hardware encoder (VideoToolbox, Media
Foundation, NVENC, VAAPI, V4L2 M2M) with a built-in H.264 software fallback;
audio is Opus. The camera is opened only while someone is watching, and
`--bitrate` is the opening ceiling. Backends with live bitrate control lower it
to fit the connection's bandwidth estimate. `moq devices` prints every source
id. Requires the `capture` feature; on Linux that needs libclang, V4L2, and
ALSA headers.

## Transcode

```bash
moq --client-connect https://relay.example.com/anon --broadcast cam.hang transcode
moq ... transcode --rung 720:2500000 --rung 360:600000 --encoder nvenc --decoder nvdec
```

Publishes `cam.hang/transcode.hang` whose catalog references the source's
rendition and adds lower rungs that are decoded and encoded only while someone
watches them. On NVIDIA the whole pipeline stays on the GPU. Requires the
`transcode` feature.

The ladder is sized against the source picture and follows it, so a source that
changes resolution mid-stream (a window capture renegotiated by a resize, a
publisher reconnecting at a new size) resolves the rungs again. Rungs that still
fit keep serving. A rung the new picture has no room for finishes its track, as
does one whose own picture moved, and the latter comes back under a new name
(`video/360p.2`), so a viewer on either reselects as it would on any other
rendition change.

## Multiple stages

Separate stages with `--` to bridge several broadcasts, or both directions,
over one connection:

```bash
moq --client-connect https://relay.example.com/anon \
    import --broadcast event.hang srt --listen 0.0.0.0:9000 \
    -- export --broadcast event.hang hls --listen 0.0.0.0:8080
```

## Redundant publishers

Two publishers that share an origin id (`--origin 42`) are treated as
interchangeable sources: relays hold both routes and fail over at a group
boundary. They must produce identical tracks with aligned groups. Everywhere
else leave `--origin` unset: a fresh id per run is what makes a restarted
encoder take over cleanly instead of splicing mid-stream.

## Tokens

```bash
moq token generate --algorithm ES256 --out private.jwk --public public.jwk
moq token sign --key private.jwk --root rooms/123 --publish alice --subscribe "" > alice.jwt
moq token verify --key public.jwk --in alice.jwt
```

See [Authentication](/bin/relay/auth).

## Retention and latency

`import --latency-max` (default 30 s) tells relays how long to keep old
groups fetchable, which the [HLS gateway](/bin/hls) depends on. `export --latency-max` (default 500 ms) is how long *this* consumer waits for a
stalled group before skipping. Raising the first never delays playback.

## Debugging

`RUST_LOG=debug` prints the negotiated version and every subscription.
`curl http://relay:4443/announced/` confirms the relay is reachable and shows
what it holds. Connection refused means UDP isn't getting through; certificate
errors on a dev relay want `--client-tls-disable-verify` or the `http://`
fingerprint flow.
