---
title: OBS Plugin
description: Stream from OBS Studio to MoQ, or bring a broadcast into a scene
---

# OBS Plugin

The plugin adds a **MoQ** streaming service and a **MoQ Source** to a stock
OBS Studio install.

- **Publish**: Settings > Stream, choose "MoQ", enter the relay URL (with `?jwt=` if needed) and broadcast path, Start Streaming.
- **Subscribe**: add a "MoQ Source", enter the relay URL and broadcast path, and the stream appears in the scene.
- **Dock**: a **MoQ** dock with Go Live, its own encoding settings, and live stats (negotiated draft, RTT, bandwidth, loss).

Enter a relay URL explicitly. On the shared anonymous relay, use a unique path
such as `https://cdn.moq.dev/anon/your-stream`. A URL with `?jwt=` fills the
token field.

By default the dock tunes the encoder for low latency on this stream only:
no B-frames or lookahead on the encoders that support turning them off. This
trades compression for less buffering; it does not guarantee an end-to-end
delay. Pick **Keep encoder settings** to leave the encoder alone.

## Source quality and moq-transcode

OBS publishes **one** rendition and does not encode a viewer ladder. When a
relay or moq.pro runs [`moq-transcode`](/bin/cli#transcode), the ladder appears
beside the source as `{broadcast}/transcode.hang`. For a good ladder, name the
broadcast with a `.hang` suffix, use a canvas at least as tall as the top rung
(1080p by default), and a source bitrate above that rung (the Quality profile's
8 Mbps clears the default 5 Mbps 1080p rung).

Local check before moq.pro (requires a CLI built with the `transcode` feature):

```bash
cargo install --locked moq-cli --features transcode
# OBS Go Live to e.g. my-obs.hang on a local relay, then:
moq --connect http://localhost:4443/anon --broadcast my-obs.hang transcode
# Watch my-obs.hang/transcode.hang
```

## Install

Prebuilt archives for macOS (Apple Silicon) and Windows (x64) are attached to
each [`obs-moq` release](https://github.com/moq-dev/moq/releases?q=obs-moq).
Extract into your OBS plugins directory. The archives are unsigned, so
Gatekeeper and SmartScreen warn on first load. Linux builds from source:

```bash
nix develop .#obs
just obs build
```

macOS and Windows source builds use `just obs setup && just obs build` with
Xcode or Visual Studio 2022; see
[`cpp/obs/`](https://github.com/moq-dev/moq/tree/main/cpp/obs).

## Advanced settings

Off by default; the defaults suit a normal relay. They cover what you would
otherwise pass to `moq` on the command line: pinning a draft or QUIC backend,
trusting a self-signed relay or private CA, reconnect pacing, congestion
control, and qlog traces. A rejected value stops the stream with the reason in
the log rather than silently using a default.

The plugin is C++ over [moq-c](/lib/c/)'s C ABI and ships with every moq-c
release.
