---
title: OBS Plugin
description: Stream from OBS Studio to MoQ, or bring a broadcast into a scene
---

# OBS Plugin

The plugin adds a **MoQ** streaming service and a **MoQ Source** to a stock
OBS Studio install.

- **Publish**: in the **MoQ** dock, enter the relay URL (with `?jwt=` if needed) and broadcast name, Go Live. OBS's own Start Streaming publishes over MoQ once obs-websocket sets the stream service, as [simulcast renditions](#simulcast-renditions) shows.
- **Subscribe**: add a "MoQ Source", enter the relay URL and broadcast path, and the stream appears in the scene.
- **Dock**: a **MoQ** dock with Go Live, encoding that uses the OBS Output settings or custom settings for this stream only, and live stats (protocol, RTT, bandwidth, loss), and the reason while a stream connects or reconnects.

Each Start Streaming publishes under a fresh instance, which the dock shows. The client's automatic
reconnects keep the same instance. A
stop/start, or an OBS auto-reconnect after the client gives up, republishes
the broadcast under a new instance, which viewers see as a new stream.

MoQ Source plays the best enabled audio and video rendition it can decode and place. It follows catalog changes while keeping the current audio and video playing until a replacement produces decoded output. A decoder setup refusal tries the next ranked rendition, including when a supported codec name carries a description the decoder cannot use. A later peer or decode failure leaves the current rendition playing. Disabling or removing every rendition of a media kind clears it. It also follows a restarted or replaced publisher, including a sibling broadcast the catalog points at, playing the new one from its first keyframe.

The source plays Opus, AAC-LC (mono or stereo), and PCM audio. A channel count names its speakers by the WAVE convention: 1, 2, 3 (2.1), 6 (5.1), and 8 (7.1) play on the matching OBS layout, while quad and 5.0 are upmixed to 5.1 and 6.1 to 7.1, which OBS lacks. Other counts are refused.

Enter a relay URL explicitly. On the shared anonymous relay, use a unique path
such as `https://cdn.moq.dev/anon/your-stream`. A URL with `?jwt=` fills the
token field. Editing the token while live presents the new one without
reconnecting, on a session that carries in-band authorization, which only
`moq-lite-07-wip` does. The default protocol does not yet, so there the dock reports the new token as refused and it applies on the
next reconnect or Go Live.

By default the dock tunes the encoder for low latency on this stream only:
no B-frames or lookahead on the encoders that support turning them off, and a
keyframe at least every 2 s, since a viewer joins at a keyframe. This trades
compression for less buffering; it does not guarantee an end-to-end delay.
Pick **Keep encoder settings** to leave the encoder alone.

OBS's own **Start Streaming** to the MoQ service tunes the profile's stream
encoder the same way, except in Advanced output mode with **Apply service
settings** off. A keyframe interval shorter than 2 s stays. Both tune x264,
NVENC, Quick Sync, AMF, VA-API, and VideoToolbox, and leave any other encoder's
settings alone.

Every video encoder the MoQ output starts has a cap, and the catalog's `bitrate`
is that cap, never the measured rate, so viewers price the rendition by what it
can send. The cap is the encoder's bitrate setting (NVENC's `max_bitrate` when its
VBR or constant quality sets one). The plugin writes it into the encoder itself, so
it holds whatever else the settings say:

- x264 keeps CBR and otherwise runs constant quality (VBR) under a VBV whose max
  rate and buffer are the cap. A CRF of 0 and a buffer size of your own are
  replaced. Your x264 options are never rewritten, so `vbv-maxrate`,
  `vbv-bufsize`, `bitrate`, `qp`, or a lossless `crf=0` in them can override the
  cap: the log warns, and the dock's Quality and Performance profiles hold it
  without them.
- NVENC keeps CBR and VBR, and otherwise runs constant quality (CQVBR), with
  `max_bitrate` at the cap and VBR's average under it. Its `opts` are never
  rewritten either, so `maxBitRate`, `averageBitRate`, or `vbvBufferSize` in them
  can override the cap, and the log warns.
- Every other encoder runs CBR at the cap: Quick Sync's ICQ and AMF's QVBR have
  no hard peak, VA-API's QVBR overshot the cap on the Mesa (AMD) and Intel
  drivers we measured, and OBS 32.2.2's VideoToolbox encoder passes its
  **Limit bitrate** to macOS as an empty limit, so its CRF runs with no peak. On an Intel Mac, OBS runs VideoToolbox's CBR as an average,
  so the cap holds only on average there.
  VA-API's `ffmpeg_opts` are never rewritten either, so `rc_mode` or `qp` in
  them can override the cap, and the log warns.

The log names each setting of yours it replaced. An encoder shared with a
recording takes the cap too, unless the recording started first: a running
encoder cannot switch rate control, so its settings stay as they are and the
catalog advertises the cap it runs with. If it runs without one (constant
quality such as CQP or CRF, lossless, or x264 with a buffer size of your own set
to 0, which turns its VBV off), the output refuses to start. Stop the
recording first, or give it its own encoder.

The dock's Custom settings profiles start from
constant quality under the cap on x264 and NVENC: Quality caps at 8 Mbps and
Performance at 2.5 Mbps, which a complex scene reaches and a calm one stays well
under. **Use OBS Output settings** starts from the rate control OBS configured.

The video bitrate follows the connection's bandwidth estimate, with the cap as
the ceiling: it drops as soon as the uplink narrows and climbs back over a few
seconds. Audio holds its configured bitrate, and video gets what is left. Only
encoders that can retune while live follow. The dock's Stats tab shows the
current video target beside the configured rate. An encoder shared with a
recording follows too, so record with a separate encoder to keep the recording
at the full rate.

## Simulcast renditions

Every video encoder on OBS's stream output is its own rendition of the
broadcast, with its size, codec, and cap in the
catalog, so `<moq-watch>` picks between them. OBS adds encoders from its WHIP
simulcast layer count, `Stream1` / `WHIPSimulcastTotalLayers` in the profile,
whatever the service: `N` layers add `N - 1` copies of the stream encoder,
layer `i` at `i/N` of the output size and bitrate. 3 layers on a 1080p
6000 kbps stream add 720p at 4000 kbps and 360p at 2000 kbps.

OBS's Settings window shows that count only for WHIP and cannot select the
MoQ service, so set both over obs-websocket, or let
[`@moq/obs`](#set-up-from-a-web-page) do it:

1. `SetStreamServiceSettings` with `streamServiceType` `moq_service` and
   `streamServiceSettings` `{"server": "<relay URL>", "key": "<broadcast path>", "bearer_token": ""}`.
   OBS merges into a service of the same type, so an empty `bearer_token`
   clears one set earlier.
2. `SetVideoSettings`, if the output size or frame rate changes.
3. `SetProfileParameter` with `Stream1`, `WHIPSimulcastTotalLayers`, and a
   count such as `"3"`.
4. Switch to another profile and back with `SetCurrentProfile`. OBS builds
   the layers when it loads a profile, so the count applies only then (or on
   restart). Loading also follows a `SetVideoSettings`: OBS 32.2.2 leaves
   existing layers on the old video, then sends no frames and crashes when
   the stream stops.
5. `StartStream`.

Saving Settings > Stream afterwards replaces the MoQ service. The dock's Go
Live publishes one rendition. OBS's Enhanced Broadcasting (multitrack video)
always streams through its RTMP output, so it cannot drive MoQ.

A rendition above a lower-bitrate one stops sending while its share of the
estimate cannot fund it, advertised as disabled so viewers switch down. Only an encoder
that follows the estimate, as above, stops; one that cannot retune while live,
or uses CQP or CRF, keeps sending. A stopped encoder keeps encoding, since OBS
cannot pause one encoder of a live output, and resumes at that encoder's next
keyframe once the estimate recovers. This needs the session to probe for
bandwidth, which takes moq-quic with BBR3.

## Set up from a web page

[`@moq/obs`](/lib/js/api/obs) runs the steps above from a page through OBS's
built-in WebSocket server (Tools > WebSocket Server Settings), with the
password the user set there. It puts a MoQ relay, or an RTMP(S) server for a
stock OBS, in a dedicated **MoQ** profile so the user's other profiles keep
their settings, follows the stream's state and stats, and adds MoQ Sources.

The MoQ service tunes the stream encoder for low latency (no B-frames or
lookahead, a 2 s keyframe interval) on x264, NVENC, Quick Sync, AMF, VA-API, and
VideoToolbox, though VideoToolbox may still buffer. An RTMP(S) server leaves that to the profile,
which over obs-websocket can tune only x264. `configure` names what the
encoder keeps at its defaults.

Chrome 147+ and Firefox ask the user before a public page reaches OBS on
loopback. Safari blocks it from HTTPS pages as mixed content.

## Source quality and moq-transcode

Without simulcast layers, OBS publishes **one** rendition. When a
relay or moq.pro runs [`moq-transcode`](/bin/cli#transcode), the ladder appears
beside the source as `{broadcast}/transcode.hang`. For a good ladder, name the
broadcast with a `.hang` suffix, use a canvas at least as tall as the top rung
(1080p by default), and a source bitrate above that rung (the Quality profile's
8 Mbps clears the default 6 Mbps 1080p rung).

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
keep-alive, segmentation offload, MTU discovery, and qlog traces), and the
WebSocket fallback race. A keep-alive interval of 0
uses the negotiated idle timeout to choose how often to ping. A rejected value stops the
stream with the reason in the log rather than silently using a default.
To decrypt a packet capture, turn on **TLS key log** here or in a MoQ Source's
properties and start OBS with `SSLKEYLOGFILE` naming the file; the variable
alone writes nothing, and the toggle without it fails the connection. Existing Unix log files must be private (mode 0600); files accessible to group or others are refused.

The plugin is C++ over the [generated C++ bindings](/lib/cpp/) and ships with
every moq-cpp release. Stopping finishes the tracks and drains for up to one
second. The default lite-06 waits for the peer to receive the tails.

If the relay closes the session for too many requests, reduce the broadcasts
or subscribed tracks, or have the operator adjust its
[session limits](/bin/relay/config#session-and-peer). Reconnecting with the same
workload does not resolve a request cap.
