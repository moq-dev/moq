---
title: OBS Plugin
description: Stream from OBS Studio to MoQ, or bring a broadcast into a scene
---

# OBS Plugin

The plugin adds a **MoQ** streaming service and a **MoQ Source** to a stock
OBS Studio install.

- **Publish**: Settings > Stream, choose "MoQ", enter the relay URL (with `?jwt=` if needed) and broadcast path, Start Streaming.
- **Subscribe**: add a "MoQ Source", enter the relay URL and broadcast path, and the stream appears in the scene.
- **Dock**: a **MoQ** dock with Go Live, encoding that uses the OBS Output settings or custom settings for this stream only, and live stats (protocol, RTT, bandwidth, loss), and the reason while a stream connects or reconnects.

Each Start Streaming publishes a fresh epoch, shown in the dock's Stats tab.
The client's automatic reconnects keep the same epoch. A stop/start, or an OBS
auto-reconnect after the client gives up, republishes the broadcast under a new
epoch, which peers using an epoch-capable protocol see as a new publisher run.

MoQ Source follows catalog changes while keeping the current audio and video playing until a replacement produces decoded output. A failed replacement leaves the current rendition playing; removing a media kind from the catalog clears it. It also follows a restarted or replaced publisher, playing the new one from its first keyframe.

The source plays Opus, AAC-LC (mono or stereo), and PCM audio. A channel count names its speakers by the WAVE convention: 1, 2, 3 (2.1), 6 (5.1), and 8 (7.1) play on the matching OBS layout, while quad and 5.0 are upmixed to 5.1 and 6.1 to 7.1, which OBS lacks. Other counts are refused.

Enter a relay URL explicitly. On the shared anonymous relay, use a unique path
such as `https://cdn.moq.dev/anon/your-stream`. A URL with `?jwt=` fills the
token field. Editing the token while live presents the new one without
reconnecting, on a session that carries in-band authorization: a
`moq-transport` draft-17 or later version pinned under Advanced settings, with
a relay that negotiates the MoQ Auth extension. The default protocol does not
yet, so there the dock reports the new token as refused and it applies on the
next Go Live.

By default the dock tunes the encoder for low latency on this stream only:
no B-frames or lookahead on the encoders that support turning them off. This
trades compression for less buffering; it does not guarantee an end-to-end
delay. Pick **Keep encoder settings** to leave the encoder alone.

The video bitrate follows the connection's bandwidth estimate, with the
configured bitrate as the ceiling: it drops as soon as the uplink narrows and
climbs back over a few seconds. Audio holds its configured bitrate, and video
gets what is left. Only encoders that can retune while live, with CBR, VBR, or
ABR rate control, follow; CQP and CRF keep their setting. The dock's Stats tab
shows the current video target beside the configured rate. An encoder shared
with a recording follows too, so record with a separate encoder to keep the
recording at the full rate.

## Source quality and moq-transcode

OBS publishes **one** rendition and does not encode a viewer ladder. When a
relay or moq.pro runs [`moq-transcode`](/bin/cli#transcode), the ladder appears
beside the source as `{broadcast}/transcode.hang`. For a good ladder, name the
broadcast with a `.hang` suffix, use a canvas at least as tall as the top rung
(1080p by default), and a source bitrate above that rung (the Quality profile's
8 Mbps clears the default 5 Mbps 1080p rung).

Local check before moq.pro (requires a CLI built with the `transcode` feature, plus
`openh264` for the software encoder a GPU-less host needs):

```bash
cargo install --locked moq-cli --features transcode,openh264
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
Xcode or Visual Studio 2022, plus Rust and
[`uniffi-bindgen-cpp`](/lib/cpp/) to build the C++ package in-tree; see
[`cpp/obs/`](https://github.com/moq-dev/moq/tree/main/cpp/obs).

## Advanced settings

Off by default; the defaults suit a normal relay. They cover what you would
otherwise pass to `moq` on the command line: pinning a protocol version, the
local bind address and connect timing, trusting a self-signed relay or private
CA, reconnect pacing, QUIC tuning (congestion control, idle timeout,
keep-alive, segmentation offload, MTU discovery, and qlog traces when the build
supports them), and the WebSocket fallback race. A rejected value stops the
stream with the reason in the log rather than silently using a default.
To decrypt a packet capture, turn on **TLS key log** here or in a MoQ Source's
properties and start OBS with `SSLKEYLOGFILE` naming the file; the variable
alone writes nothing, and the toggle without it fails the connection. Existing Unix log files must be private (mode 0600); files accessible to group or others are refused.

The plugin is C++ over the [generated C++ bindings](/lib/cpp/) and ships with
every moq-cpp release. Stopping finishes the tracks and drains for up to one
second. The default lite-06 waits for transport ACKs, which do not confirm that
the peer received the tail.

If the relay closes the session for too many requests, reduce the broadcasts
or subscribed tracks, or have the operator adjust its
[session limits](/bin/relay/config#session-and-peer). Reconnecting with the same
workload does not resolve a request cap.
