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

## Prerequisites

The plugin requires GStreamer development libraries. It is **not** built by default since most users don't have them installed.

If you're using Nix, GStreamer is included in the dev shell automatically. Otherwise, install manually:

- **macOS:** `brew install gstreamer`
- **Debian/Ubuntu:** `apt install libgstreamer1.0-dev gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad`
- **Arch:** `pacman -S gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad`

## Building

```bash
cargo build -p moq-gst
```

This produces a shared library (cdylib) in `target/debug/`. GStreamer needs to find this plugin via the `GST_PLUGIN_PATH_1_0` environment variable — the `just` commands below handle this automatically.

## Running Locally

Start a [relay server](/app/relay/) first:

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
    moqsink name=mux url="http://localhost:4443/anon" broadcast="bbb"
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
  moqsrc url="http://localhost:4443/anon" broadcast="bbb" \
    ! decodebin3 ! videoconvert ! autovideosink
```

## Supported Codecs

### moqsink (publish)

| Media | Codec | GStreamer caps        |
| ----- | ----- | --------------------- |
| Video | H.264 | `video/x-h264`        |
| Video | H.265 | `video/x-h265`        |
| Video | AV1   | `video/x-av1`         |
| Audio | AAC   | `audio/mpeg` (v4)     |
| Audio | Opus  | `audio/x-opus`        |

### moqsrc (subscribe)

Outputs the same caps based on the catalog, compatible with `decodebin3`.

## Debugging

Enable GStreamer debug output:

```bash
# GStreamer debug (verbose)
GST_DEBUG=*:4 just pub gst bbb

# Rust logging
RUST_LOG=debug just pub gst bbb
```
