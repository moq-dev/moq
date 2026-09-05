---
title: GStreamer Plugin
description: moqsink and moqsrc elements
---

# GStreamer Plugin

Two elements: **moqsink** publishes a pipeline to a relay, **moqsrc**
subscribes to a broadcast and exposes one source pad per rendition.

```bash
# Inspect (Nix bundles the plugin with gst-launch)
nix shell github:moq-dev/moq#moq-gst --command gst-inspect-1.0 moq

# Play the public test broadcast
nix shell github:moq-dev/moq#moq-gst --command gst-launch-1.0 -e \
  moqsrc name=s url=https://cdn.moq.dev/demo broadcast=bbb.hang \
  s.video_0 ! queue ! decodebin3 ! videoconvert ! autovideosink \
  s.audio_0 ! queue ! decodebin3 ! audioconvert ! autoaudiosink

# Publish a test pattern
gst-launch-1.0 -e videotestsrc is-live=true ! x264enc tune=zerolatency ! h264parse \
  ! video/x-h264,stream-format=byte-stream,alignment=au ! mux.sink_0 \
  moqsink name=mux url=https://cdn.moq.dev/anon broadcast=<your-name>.hang
```

Install via `apt install gstreamer1.0-moq` or `dnf install gstreamer1-moq`
([Install](/setup/install)), or build with `cargo build -p moq-gst` and point
`GST_PLUGIN_PATH_1_0` at the output. `http://` URLs pin the relay's
certificate fingerprint automatically, so local development needs no TLS setup.
That scheme is for localhost only: the fingerprint is fetched unauthenticated
and the WebSocket fallback runs as cleartext `ws://`, so use `https://` for
anything else.

## moqsink

| Codec | Caps |
| --- | --- |
| H.264, H.265, AV1, VP8, VP9 | `video/x-h264`, `video/x-h265`, `video/x-av1`, `video/x-vp8`, `video/x-vp9` |
| AAC, MP3, Opus | `audio/mpeg`, `audio/x-opus` |
| Opaque data | `application/octet-stream` (raw bytes on a named track, one group per buffer) |

Each `sink_%u` request pad is one track. Pad properties: `track` names it
(default: after the codec), `container=loc` publishes it as
[LOC](/concept/standard#loc) instead of the legacy hang container, and
`track-status`/`track-error` report its lifecycle. Element properties:
`url`, `broadcast`, `tls-disable-verify`, `quic-idle-timeout`,
`quic-keep-alive`, and read-only `status`, `connected`, `moq-version`, and
`estimated-send-bitrate`. The sink reconnects for as long as the pipeline
runs and only reports `failed` on an answer redialing can't change, such as a
rejected token.

## moqsrc

Pads are named by kind and appear as the catalog announces renditions:
`video_0`, `video_1`, `audio_0`. Link the pad you want by name; the terse
`moqsrc ! decodebin3` form links only the first pad offered, which may be
audio. Each pad sends EOS when its rendition ends. Properties: `url`,
`broadcast`, `tls-disable-verify`.

Debug with `GST_DEBUG=*:4` for GStreamer and `RUST_LOG=debug` for the plugin.
