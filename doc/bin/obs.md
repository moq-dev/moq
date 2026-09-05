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
