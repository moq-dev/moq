---
title: OBS Plugin
description: Stream from OBS Studio to MoQ, or bring a broadcast into a scene
---

# OBS Plugin

The plugin adds a **MoQ** streaming service and a **MoQ Source** to a stock
OBS Studio install.

- **Publish**: Settings > Stream, choose "MoQ", enter the relay URL (with `?jwt=` if needed) and broadcast path, Start Streaming.
- **Subscribe**: add a "MoQ Source", enter the relay URL and broadcast path, and the stream appears in the scene.
- **Dock**: **Stream** contains the relay URL, optional publish token and
  broadcast name, Go Live, and connection state. Leave the broadcast name empty
  to publish at the relay URL path. Paste a URL with `?jwt=` to fill the token
  field automatically. Reconnect settings live under **Advanced**.
  **Encoding** chooses between **Use OBS Output settings** and **Custom settings
  for MoQ**. Both use OBS encoders; custom settings apply only to this MoQ stream.
  **Encoder latency** defaults to **Low latency** in both modes. It overrides
  buffering settings only for this MoQ stream; **Keep encoder settings** preserves
  them. x264 uses its zero-latency tune, VideoToolbox disables B-frames, NVENC
  uses ultra-low tuning without B-frames/lookahead, and Quick Sync uses ultra-low
  mode without B-frames. Other encoders retain their settings. **Stats** reports
  the applied policy. This trades compression efficiency for less buffering,
  not a guaranteed end-to-end delay: OBS VideoToolbox and NVENC can still queue
  frames internally. Keyframe join delay and viewer buffering are separate.
  Custom profiles offer Auto, Quality, or Performance, with hardware/software,
  video codec, encoder, and audio codec choices.
  **Stats** shows the active encoding, negotiated draft, dial scheme, and one
  minute of RTT, estimated send/receive bandwidth, packet loss, and bytes sent.
  **About** lists plugin and libmoq versions, documentation links, and available
  video encoders.

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
