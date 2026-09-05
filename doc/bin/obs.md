---
title: OBS Plugin
description: Stream from OBS Studio to MoQ, or bring a broadcast into a scene
---

# OBS Plugin

The plugin adds a **MoQ** streaming service and a **MoQ Source** to a stock
OBS Studio install.

- **Publish**: Settings > Stream, choose "MoQ", enter the relay URL (with `?jwt=` if needed) and broadcast path, Start Streaming.
- **Subscribe**: add a "MoQ Source", enter the relay URL and broadcast path, and the stream appears in the scene.
- **Dock**: a MoQ dock shows connection state and opens the advanced settings.
  **Stream** is Go Live, token, stats, and timeline. Status is Connecting,
  Connected, Reconnecting (libmoq is between connections), or Disconnected.
  Reconnect delay / cap / give up live under Advanced. **Quality** is the OBS
  source encode (off uses Settings > Output; on uses Auto / Quality /
  Performance plus hardware vs software and H.264 / HEVC / AV1, AAC or Opus).
  **Transcode** is a relay ladder request for [`moq-transcode`](/bin/cli#transcode)
  when moq.pro enables it: Default / Light / Mobile profiles, published beside
  the source as `{broadcast}/transcode.hang`. OBS does not encode those rungs.
  The **Version** tab lists the plugin and libmoq versions.
  **Publish token** is optional (leave empty for public relays such as `/anon`).
  Paste a URL with `?jwt=` into Relay URL and the dock peels the token into that field.
  **Show stats** (on by default) lists the active Quality encode, Transcode
  request, and negotiated MoQ draft and transport. **Show timeline** is five
  compact sparklines (RTT, send, recv, loss, bytes sent) for the last minute,
  from `moq_session_stats`. The Stream tab keeps a libmoq version footer; the
  **Version** tab links plugin / libmoq docs plus [moq.dev](https://moq.dev) and
  [moq.pro](https://moq.pro).

## Source quality and moq-transcode

OBS publishes **one** hang mezzanine. It does not encode a viewer ladder inside
the plugin. When a relay or moq.pro enables [`moq-transcode`](/bin/cli#transcode),
the ladder catalog appears beside that source as `{broadcast}/transcode.hang`
(the same path the `moq … transcode` CLI uses). Prefer a broadcast name ending
in `.hang`, a canvas at least as tall as the top rung you want (1080p for the
default ladder), and a source bitrate above the top rung ceiling (Quality
targets 8 Mbps CBR so the default 5 Mbps 1080p rung can undercut it). The catalog
carries coded size and configured bitrate so the transcoder can size rungs
before measured rates arrive.

Local check before moq.pro:

```bash
# OBS Go Live to e.g. my-obs.hang on a local relay, then:
moq --client-connect https://localhost:4443/anon --broadcast my-obs.hang transcode
# Watch my-obs.hang/transcode.hang
```

## Install

Prebuilt archives for macOS (Apple Silicon) and Windows (x64) are attached to
each [`obs-moq` release](https://github.com/moq-dev/moq/releases?q=obs-moq).
Extract into your OBS plugins directory. The archives are unsigned, so
Gatekeeper and SmartScreen warn on first load. Linux builds from source:

```bash
nix develop
just obs build
```

macOS and Windows source builds use `just obs setup && just obs build` with
Xcode or Visual Studio 2022; see
[`cpp/obs/`](https://github.com/moq-dev/moq/tree/main/cpp/obs).

## Advanced settings

Off by default; the defaults suit a normal relay. When enabled they cover the
things you'd otherwise pass to `moq` on the command line: pinning a protocol
draft or QUIC backend, trusting a self-signed relay by fingerprint or a
private CA, an SNI override, reconnect pacing, congestion control (delay-based
BBR or loss-based CUBIC), stream limits and timeouts, qlog traces for
diagnosing stalls, and the WebSocket fallback race. A rejected value stops the
stream with the reason in the log rather than silently using a default.

The plugin is C++ over [libmoq](/lib/c/)'s C ABI and ships with every libmoq
release.
