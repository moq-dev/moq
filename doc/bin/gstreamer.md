---
title: GStreamer Plugin
description: GStreamer plugin for MoQ
---

# GStreamer Plugin

A GStreamer plugin for publishing and consuming MoQ streams.

::: warning Work in Progress
This plugin is currently under development, but it works okay.
:::

## Overview

The GStreamer plugin provides two elements:

- **moqsink** - Publish media to a MoQ relay
- **moqsrc** - Subscribe to MoQ broadcasts

Both elements support the following properties:

| Property             | Type   | Description                                                       |
| -------------------- | ------ | ----------------------------------------------------------------- |
| `url`                | string | The relay URL to connect to                                       |
| `broadcast`          | string | The broadcast name                                                |
| `tls-disable-verify` | bool   | Disable TLS certificate validation (rarely needed, default false) |

::: info
For `http://` URLs, `moq-native` automatically fetches the server's certificate fingerprint from `/certificate.sha256` and verifies TLS against it. You don't need `tls-disable-verify` for local development.
:::

`moqsink` additionally exposes these read-only properties for monitoring:

| Property                 | Type   | Description                                                  |
| ------------------------ | ------ | ----------------------------------------------------------- |
| `connected`              | bool   | Whether the publish session is currently connected          |
| `moq-version`            | string | The negotiated MoQ protocol version; null when disconnected |
| `estimated-send-bitrate` | uint64 | Estimated send bitrate in bits per second; 0 when unavailable |

## Prerequisites

The plugin requires GStreamer development libraries. It is **not** built by default since most users don't have them installed.

If you're using Nix, GStreamer is included in the dev shell automatically. Otherwise, install manually:

- **macOS:** `brew install gstreamer`
- **Debian/Ubuntu:** `apt install libgstreamer1.0-dev gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad`
- **Arch:** `pacman -S gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad`

## Quick start with Nix

If you have Nix installed, you don't need to build anything or set any environment variables. The `moq-gst` flake output bundles the plugin with wrappers around `gst-inspect-1.0` / `gst-launch-1.0` that preload moq alongside `gst-plugins-{base,good,bad}`, so the standard tools find `moqsink` / `moqsrc` automatically.

### Inspect the plugin

```bash
nix shell github:moq-dev/moq#moq-gst --command gst-inspect-1.0 moq
```

Lists `moqsink` and `moqsrc`. As a one-liner: `nix run github:moq-dev/moq#moq-gst -- moq`.

### Subscribe to the public test broadcast

`cdn.moq.dev/demo` hosts an always-on `bbb.hang` broadcast (looping Big Buck Bunny). Render it to a window:

```bash
nix shell github:moq-dev/moq#moq-gst --command gst-launch-1.0 -v -e \
  moqsrc name=s url=https://cdn.moq.dev/demo broadcast=bbb.hang \
  s.video_0 ! queue ! decodebin3 ! videoconvert ! autovideosink \
  s.audio_0 ! queue ! decodebin3 ! audioconvert ! autoaudiosink
```

`bbb.hang` carries both video and audio, so each is linked by pad name (`video_0` /
`audio_0`). For video only, drop the `s.audio_0` branch; the audio pad simply stays
unlinked. The terse `moqsrc ! decodebin3 ! ...` form links just the first pad GStreamer
offers, which on a multi-track broadcast may be the audio one, so prefer naming the pad.

### Publish your own broadcast

`cdn.moq.dev/anon` accepts publishers without auth. Pick a name, publish, then subscribe to that same name (in another terminal or from another machine).

```bash
# Download a pre-fragmented CMAF test file (one time).
curl -fsSL https://vid.moq.dev/bbb.mp4 -o bbb.mp4

# Terminal 1: loop the file as a broadcast named `<your-name>.hang`.
nix shell github:moq-dev/moq#moq-gst --command gst-launch-1.0 -v -e \
  multifilesrc location=bbb.mp4 loop=true ! parsebin name=parse \
    parse. ! queue ! identity sync=true ! mux.sink_0 \
    parse. ! queue ! identity sync=true ! mux.sink_1 \
    moqsink name=mux url=https://cdn.moq.dev/anon broadcast=<your-name>.hang
```

```bash
# Terminal 2: render it.
nix shell github:moq-dev/moq#moq-gst --command gst-launch-1.0 -v -e \
  moqsrc url=https://cdn.moq.dev/anon broadcast=<your-name>.hang \
  ! decodebin3 ! videoconvert ! autovideosink
```

### Local relay

If you'd rather run a relay yourself, the [relay binary](/bin/relay/) is in the same flake:

```bash
# Terminal 1: start a relay on localhost:4443.
nix run github:moq-dev/moq#moq-relay -- demo/relay/localhost.toml

# Terminal 2: publish.
nix shell github:moq-dev/moq#moq-gst --command gst-launch-1.0 -v -e \
  multifilesrc location=bbb.mp4 loop=true ! parsebin name=parse \
    parse. ! queue ! identity sync=true ! mux.sink_0 \
    parse. ! queue ! identity sync=true ! mux.sink_1 \
    moqsink name=mux url=http://localhost:4443 broadcast=bbb.hang

# Terminal 3: subscribe.
nix shell github:moq-dev/moq#moq-gst --command gst-launch-1.0 -v -e \
  moqsrc url=http://localhost:4443 broadcast=bbb.hang \
  ! decodebin3 ! videoconvert ! autovideosink
```

::: tip
`http://` URLs auto-verify TLS via `/certificate.sha256` fingerprint pinning, so localhost development needs no certificate setup.
:::

## Building

```bash
cargo build -p moq-gst
```

This produces a shared library (cdylib) in `target/debug/`. GStreamer needs to find this plugin via the `GST_PLUGIN_PATH_1_0` environment variable — the `just` commands below handle this automatically.

## Running Locally

Start a [relay server](/bin/relay/) first:

```bash
just relay
```

### Publishing

Use the `just` shortcut to publish a test video via GStreamer:

```bash
# Publish Big Buck Bunny (downloads automatically)
just pub gst bbb

# Publish to a remote relay
just pub gst bbb https://cdn.moq.dev/anon
```

Or run `gst-launch-1.0` directly:

```bash
# Point GST_PLUGIN_PATH_1_0 at the build output
export GST_PLUGIN_PATH_1_0="$PWD/target/debug${GST_PLUGIN_PATH_1_0:+:$GST_PLUGIN_PATH_1_0}"

# Publish a fragmented MP4 file
gst-launch-1.0 -v -e \
  multifilesrc location=demo/pub/media/bbb.mp4 loop=true ! parsebin name=parse \
    parse. ! queue ! identity sync=true ! mux.sink_0 \
    parse. ! queue ! identity sync=true ! mux.sink_1 \
    moqsink name=mux url="http://localhost:4443" broadcast="bbb"
```

::: tip
The input video must be a fragmented MP4 (CMAF). The `just pub download` helper fetches pre-fragmented test videos from `vid.moq.dev`. To fragment your own video:

```bash
ffmpeg -i input.mp4 -c copy \
  -f mp4 -movflags cmaf+separate_moof+delay_moov+skip_trailer+frag_every_frame \
  output.mp4
```

:::

### Subscribing

```bash
# Subscribe and render to the screen
just sub gst bbb

# Subscribe from a remote relay
just sub gst bbb https://cdn.moq.dev/anon
```

Or directly:

```bash
export GST_PLUGIN_PATH_1_0="$PWD/target/debug${GST_PLUGIN_PATH_1_0:+:$GST_PLUGIN_PATH_1_0}"

gst-launch-1.0 -v -e \
  moqsrc url="http://localhost:4443" broadcast="bbb" \
    ! decodebin3 ! videoconvert ! autovideosink
```

::: warning
`moqsrc` exposes one source pad per rendition: `video_0`, `audio_0`, and so on
(see [moqsrc pads](#moqsrc-subscribe)). The single-branch `moqsrc ! decodebin3 ...`
above only links the *first* pad GStreamer offers, so on a broadcast with both video
and audio it may pick up the audio pad and a video-only sink chain then renders nothing.
Link the pad you want by name, and route the rest to a sink so they don't stall:

```bash
gst-launch-1.0 -v -e moqsrc name=s url="http://localhost:4443" broadcast="bbb" \
  s.video_0 ! queue ! decodebin3 ! videoconvert ! autovideosink \
  s.audio_0 ! queue ! decodebin3 ! audioconvert ! autoaudiosink
```

The first pad of each kind is always `video_0` / `audio_0` regardless of catalog order.
:::

## Supported Codecs

### moqsink (publish)

| Media | Codec | GStreamer caps        |
| ----- | ----- | --------------------- |
| Video | H.264 | `video/x-h264`        |
| Video | H.265 | `video/x-h265`        |
| Video | AV1   | `video/x-av1`         |
| Video | VP8   | `video/x-vp8`         |
| Video | VP9   | `video/x-vp9`         |
| Audio | AAC   | `audio/mpeg` (v4)     |
| Audio | MP3   | `audio/mpeg` (v1/v2, layer 3) |
| Audio | Opus  | `audio/x-opus`        |

### moqsrc (subscribe)

Outputs the same caps based on the catalog, compatible with `decodebin3`.

One source pad is created per rendition, named after its kind: `video_0`, `video_1`,
`audio_0`, and so on. The first pad of each kind is always numbered `0`, so a
`gst-launch` pipeline can link the stream it wants by name (`moqsrc name=s s.video_0 ! ...`)
no matter which rendition the catalog announces first. Pads appear once their rendition
shows up in the catalog (sometimes-pads), so an application links them from a
`pad-added` handler.

## Debugging

Enable GStreamer debug output:

```bash
# GStreamer debug (verbose)
GST_DEBUG=*:4 just pub gst bbb

# Rust logging
RUST_LOG=debug just pub gst bbb
```
