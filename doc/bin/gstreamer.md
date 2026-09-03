---
title: GStreamer Plugin
description: GStreamer plugin for MoQ
---

# GStreamer Plugin

A GStreamer plugin for publishing and consuming MoQ streams.

::: warning Active development
The plugin is usable, but its API may still change.
:::

## Elements and properties

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
For `http://` URLs, `moq-tokio` automatically fetches the server's certificate fingerprint from `/certificate.sha256` and verifies TLS against it. You don't need `tls-disable-verify` for local development.
:::

`moqsink` additionally supports these QUIC connection properties:

| Property            | Type   | Description                                                     |
| ------------------- | ------ | --------------------------------------------------------------- |
| `quic-idle-timeout` | uint64 | Idle timeout in milliseconds (default 30000; 0 disables locally) |
| `quic-keep-alive`   | uint64 | Keep-alive interval in milliseconds (default 5000; 0 disables)   |

An idle timeout of `0` disables it locally.
These values apply only to QUIC connections. WebSocket fallback uses its own heartbeat policy.
The iroh backend applies the idle timeout but ignores keep-alive because it has no keep-alive setting.

```bash
gst-launch-1.0 -e \
  videotestsrc is-live=true ! x264enc tune=zerolatency ! h264parse \
    ! video/x-h264,stream-format=byte-stream,alignment=au ! mux.sink_0 \
  moqsink name=mux url=http://localhost:4443 broadcast=test \
    quic-idle-timeout=15000 quic-keep-alive=3000
```

`moqsink` additionally exposes these read-only properties for monitoring. Each emits a `notify`
signal when it changes, so you can poll it via `g_object_get` or connect to `notify::<property>`:

| Property                 | Type   | Description                                                  |
| ------------------------ | ------ | ----------------------------------------------------------- |
| `status`                 | enum   | Publish connection lifecycle: `disconnected` (retrying), `connected`, or `failed` (gave up) |
| `connected`              | bool   | Whether the publish session is currently connected (`status == connected`) |
| `moq-version`            | string | The negotiated MoQ protocol version; null when disconnected |
| `estimated-send-bitrate` | uint64 | Estimated send bitrate in bits per second (congestion controller); 0 when unavailable |
| `estimated-recv-bitrate` | uint64 | Estimated receive bitrate in bits per second; 0 when unavailable |

`status` distinguishes a drop the reconnect loop is still retrying (`disconnected`) from a permanent
give-up (`failed`), which a bare `connected` bool cannot. The sink retries for as long as the
pipeline runs, so a relay outage of any length is ridden out; it goes `failed` only on an answer the
relay actually gave that redialing cannot change: a rejected token, or a CONNECT answered with a
status that isn't an invitation to retry. Everything else keeps retrying, so watch the logs when a
sink stays `disconnected` from the very first attempt: a pipeline that has never connected once is
far more likely misconfigured than waiting out an outage.

## Prerequisites

The plugin requires GStreamer development libraries. It is **not** built by default since most users don't have them installed.

If you're using Nix, GStreamer is included in the dev shell automatically. Otherwise, install manually:

- **macOS:** `brew install gstreamer`
- **Debian/Ubuntu:** `apt install libgstreamer1.0-dev gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad`
- **Arch:** `pacman -S gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad`

## Quick start with Nix

On Linux or Apple Silicon macOS, Nix avoids a manual build and environment variables. The `moq-gst` flake output bundles the plugin with wrappers around `gst-inspect-1.0` / `gst-launch-1.0` that preload moq alongside `gst-plugins-{base,good,bad}`, so the standard tools find `moqsink` / `moqsrc` automatically.

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

This produces a shared library (cdylib) in `target/debug/`. GStreamer needs to find this plugin via the `GST_PLUGIN_PATH_1_0` environment variable; the `just` commands below handle this automatically.

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
| Text  | Captions | `text/x-raw` (utf8) |
| Data  | Opaque | `application/octet-stream` |

#### Captions

A `text/x-raw` pad is published as a hang text rendition, one WebVTT cue per group. Each buffer is
one decoded cue: the PTS is the cue's start on the same clock as audio and video, and the buffer
duration is its end, so a cue without a duration is dropped rather than left on screen forever.

This is where captions come from, because ffmpeg cannot mux a subtitle track into fragmented MP4
and so the `moq import fmp4` path can't carry one. A demuxer that resolves timed text for you
(`qtdemux` on a 3GPP timed-text track, for example) can feed the pad directly:

```sh
gst-launch-1.0 -e filesrc location=input.mp4 ! qtdemux name=demux \
    demux.video_0 ! queue ! h264parse ! identity sync=true ! mux.sink_0 \
    demux.audio_0 ! queue ! aacparse ! identity sync=true ! mux.sink_1 \
    demux.subtitle_0 ! queue ! identity sync=true ! mux.sink_2 \
    moqsink name=mux url="http://localhost:4443" broadcast="example.hang"
```

Cue text is escaped before it goes on the wire, so markup in the source shows as literal text
rather than opening a WebVTT tag. Note that `gst-launch` builds every named branch up front: point
`demux.subtitle_0` at a file with no text track and that branch never links, leaving its queue
without EOS so the pipeline will not shut down.

Each `sink_%u` request pad publishes one track. By default the track is named after its codec
(`0.avc3`, `0.aac`, and so on) and the catalog advertises that name. To choose the name, set the pad's
`track` property through `GstChildProxy`:

```bash
gst-launch-1.0 -v -e \
  multifilesrc location=bbb.mp4 loop=true ! parsebin name=parse \
    parse. ! queue ! identity sync=true ! mux.sink_0 \
    parse. ! queue ! identity sync=true ! mux.sink_1 \
  moqsink name=mux url=http://localhost:4443 broadcast=bbb.hang \
    sink_0::track=camera sink_1::track=commentary
```

Media tracks use the legacy Hang container by default. Set a pad's `container=loc` before its CAPS
event to publish that track as Low Overhead Container and advertise LOC in the catalog:

```bash
gst-launch-1.0 -v -e \
  videotestsrc is-live=true ! x264enc tune=zerolatency ! h264parse \
    ! video/x-h264,stream-format=byte-stream,alignment=au ! mux.sink_0 \
  moqsink name=mux url=http://localhost:4443 broadcast=bbb.hang sink_0::container=loc
```

Set the property on each media pad that should use LOC; other pads remain Legacy.

Like `track`, `container` is writable in any state until the pad's CAPS event reserves the track.
It is fixed for that producer and becomes writable again after returning to `READY`.

`track` is writable in any state until the pad's CAPS event reserves the track, so a pad requested
while the pipeline runs can still be named before its first buffer. From then on it reads back the
reserved name, the generated one included, and further writes are ignored with a warning; stopping the
element (back to `READY`) releases the reservation and makes it writable again. An empty string keeps
the generated name. A name another pad already holds invalidates only that pad, so the rest of the
broadcast keeps publishing.

Each pad also reports what its track is doing, so a publication can be diagnosed without reading the
logs or asking a consumer:

| Property      | Type   | Description                                       |
| ------------- | ------ | ------------------------------------------------- |
| `track-status` | enum   | `pending` until CAPS builds a producer, `active` once the broadcast reserved the track, `ended` when it was finalized, `error` when the pad was invalidated |
| `track-error` | string | Why the pad was invalidated, null when it was not  |

Both emit `notify`, so an application can connect to `notify::track-status` rather than poll. `active`
means the producer exists and the track is registered, not merely that the pad was requested, and a
pad that sent EOS stays `active` until every pad has ended and the producers are finalized. `error`
is terminal: it survives EOS, and clears when the pad is released or the element goes back to
`READY`. A pad still waiting for CAPS is `pending` with no error, because nothing has failed yet.
Connection loss is the element's own `status`.

A pad negotiated as `application/octet-stream` publishes application data instead of media. The bytes
go out exactly as they arrive: no codec, no media container, no interpretation. A data pad's
`container` property has no effect.

```bash
gst-launch-1.0 -v -e \
  appsrc name=levels format=time do-timestamp=true caps=application/octet-stream ! mux.sink_0 \
  moqsink name=mux url=https://cdn.moq.dev/anon broadcast=bbb.hang \
    sink_0::track=audiolevels
```

Such a pad requires `track`: an opaque track is advertised nowhere, so a generated name would be
unreachable, and a pad without one is invalidated. Its segments must use TIME format at rate `1.0`
and must not rewind running time without a flush. Byte-oriented sources such as `filesrc` and `fdsrc`
push a BYTES segment, and every buffer behind one is dropped with a single warning on the bus.

Each accepted buffer becomes one group holding one frame, stamped with the buffer's PTS mapped through
that segment. A buffer without a PTS uses the pipeline's current running time when available. If running
time is unavailable, timestamping fails. Frames larger than the group cache limit are rejected before
their group is created.

The track is deliberately absent from the catalog. MSF requires a `packaging` value on every declared
track and defines none for raw bytes, so a consumer is told out of band, by configuration, which data
tracks to subscribe to.

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
