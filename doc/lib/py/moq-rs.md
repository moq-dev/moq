---
title: moq-rs (Python)
description: Python pub/sub for Media over QUIC
---

# moq-rs

[![PyPI](https://img.shields.io/pypi/v/moq-rs)](https://pypi.org/project/moq-rs/)
[![Documentation](https://readthedocs.org/projects/moq-rs/badge/?version=latest)](https://moq-rs.readthedocs.io)

Async pub/sub for [Media over QUIC](/) in Python.

Full API reference: [moq-rs.readthedocs.io](https://moq-rs.readthedocs.io), built automatically from the docstrings on each commit.

The underlying transport is the Rust [`moq-net`](/lib/rs/crate/moq-net) crate, exposed through UniFFI (the [`moq-ffi`](https://pypi.org/project/moq-ffi/) package) and wrapped in a Pythonic API: no `Moq` prefixes on user-facing types, async iterators for streams, async context managers for sessions. `moq-rs` is versioned independently of `moq-ffi` and floats to the latest compatible patch.

## Install

```bash
pip install moq-rs
```

Requires Python 3.10+. The distribution is `moq-rs` (the `moq` name is taken on PyPI); the import name is `moq`. Installing it pulls in the `moq-ffi` native bindings automatically.

## Concepts

A **broadcast** is a collection of tracks identified by a path. A **track** is a live stream of frames. Producers create broadcasts on an origin; consumers subscribe to whatever has been announced.

`create_broadcast(path)` creates an announced broadcast on the origin and returns its producer. Toggle discoverability with `set_announce(False)` (the broadcast stays reachable by exact path), and call `finish()` to unpublish it.

For unstructured byte streams (status, commands, sensor data), use `publish_track` / `subscribe_track`. For encoded media, use `publish_audio` or `publish_video` to publish one rendition, or `publish_container` to hand over a container (fMP4, MKV, TS, FLV) that describes its own tracks. Either way the catalog is populated automatically, and `subscribe_media` reads it back.

## API summary

### Connection

```python
async with moq.Client("https://relay.example.com") as client:
    ...
```

`Client(url, *, tls_verify=True, tls_roots=None, tls_system_roots=None, tls_fingerprints=None, tls_cert=None, tls_key=None, bind=None, reconnect=True, backoff=None, publish=None, subscribe=None)`. Use `tls_cert` and `tls_key` for mutual TLS. Without `publish` / `subscribe` an internal origin is created automatically. Pass an `OriginProducer` to share state across multiple clients.

The session automatically reconnects with backoff when the transport drops (a relay restart, a laptop waking from sleep), and broadcasts consumed through it ride out the gap. Pass `reconnect=False` for a one-shot dial, or `backoff=moq.Backoff(initial_ms=500, multiplier=2, max_ms=10_000, timeout_ms=0)` to tune the pacing (`timeout_ms=0` retries forever). Observe the transitions with `Session.status()`:

```python
while True:
    status = await client.session.status()  # CONNECTED / DISCONNECTED / MIGRATING
```

`Session.closed()` resolves only when the connection stops for good: it raises the terminal error when the retries are exhausted, and returns normally after a local shutdown.

Inspect a server request's logical endpoint before accepting it with the query-free `request.path`. It is consistent across transports and returns `""` for the root or missing path. `request.query` returns the encoded query and may contain credentials:

```python
import asyncio

active = set()
async with moq.Server("127.0.0.1:4443", tls_generate=["localhost"]) as server:
    async for request in server:
        if request.path == "/admin":
            await request.reject(403)
            continue
        session = await request.accept()
        closed = asyncio.create_task(session.closed())
        active.add(closed)
        closed.add_done_callback(active.discard)

await asyncio.gather(*active, return_exceptions=True)
```

A server can reject the connection on auth grounds: `moq.Error.Unauthorized` (HTTP 401) or `moq.Error.Forbidden` (HTTP 403). These are terminal, so handle them separately from a transient transport failure rather than reconnecting. `moq.is_auth(err)` catches both:

```python
try:
    async with moq.Client("https://relay.example.com") as client:
        ...
except moq.Error as err:
    if moq.is_auth(err):
        ...  # Prompt for credentials; don't reconnect.
    raise
```

`moq.is_shutdown(err)` is the companion: true for `Cancelled` and `Closed`, which arise from graceful shutdown rather than an actual failure. Use it to break out of an `async for` without treating the expected end-of-stream error as a problem.

### Publishing media

```python
broadcast = client.create_broadcast("my-stream")
audio = broadcast.publish_audio(moq.AudioFormat.OPUS, opus_init_bytes, label="English")

# Audio has no keyframes, so `cut` is what gives it group boundaries. Once per
# frame is the lowest latency; a segment cadence suits HLS/DASH.
audio.write_frame(payload, timestamp_us=0)
audio.cut()
audio.finish()
broadcast.finish()
```

Supported codec formats include `opus`, `avc3`, `hev1`, `av01`, `vp09`, and others. See [`hang`](/lib/rs/crate/hang) for the full list.
The optional `label` is presentation metadata for a track picker. The transport
track name remains generated from the format and labels do not need to be unique.
`publish_container` takes neither a label nor a hint: a container describes each
track it publishes from its own metadata.

### Publishing raw media

The calls above take frames you already encoded. To hand over raw pixels or PCM instead and let the codec run inside the bindings, use `encode_video` / `encode_audio`. Pixel format, resolution, and framerate are fixed at publish time, so each frame carries only its pixels and a timestamp:

```python
video = broadcast.encode_video(
    moq.VideoEncoderInput(
        format=moq.VideoPixelFormat.RGBA,
        width=1280,
        height=720,
        framerate=30,
    ),
    moq.VideoEncoderOutput(
        codec=moq.VideoCodec.H264,
        kind=moq.VideoEncoderKind.AUTO(),
    ),
)

video.write(moq.VideoFrame(timestamp_us=pts_us, data=rgba))
video.finish()
```

`VideoEncoderKind.AUTO()` prefers a hardware encoder and falls back to software; `SOFTWARE()`, `HARDWARE()`, and `NAMED("videotoolbox")` pin the choice (each variant is a class, so call it). The bindings compile VideoToolbox (macOS), Media Foundation (Windows), and openh264 (software, everywhere); the Linux hardware codecs are a libmoq-only build option. `set_bitrate` retunes the live encoder without forcing a keyframe, cheap enough to drive from a congestion controller.

The track is named after the codec (`.avc3` / `.hev1`) and its catalog rendition is published immediately, read out of the encoder itself, so subscribers discover it through the catalog rather than a name you pick, and can find it before the first frame exists. `cut()` starts a new group at the next frame, which is optional: the encoder keyframes every `gop` frames on its own, and each of those cuts a group.

`decode_video` and `decode_audio` are the mirrors: they run the codec inside the bindings on the way in, so a subscriber gets pixels and PCM without linking one. Video frames arrive as tightly-packed I420 and carry the size they actually decoded to, since `resize` is only best effort:

```python
catalog = await broadcast.catalog()
name, rendition = next(iter(catalog.video.items()))

async with await broadcast.decode_video(name, rendition) as video:
    async for frame in video:
        render(frame.data, frame.width, frame.height)
```

An unrecognized codec raises from `decode_video` itself; a recognized one no native backend handles raises when the decoder opens. Either way you find out before the first frame.

The catalog is filled by parsing the codec bitstream. `publish_video` takes an optional `hint` to supply fields the stream can't reveal (such as `bitrate`), or to publish the catalog before the first keyframe:

```python
video = broadcast.publish_video(
    "avc3",
    avc_init_bytes,
    video=moq.VideoHint(bitrate=4_000_000),
)
```

A value the stream later detects fills only a gap the hint left, so a detected value always wins. Audio formats resolve entirely from their init bytes, so they take no hint.

Each catalog `Video` has a `stalled` boolean. A true value recommends temporarily avoiding that rendition, but the track remains directly usable. Existing catalogs default it to false.

Properties that apply to every video rendition are updated together. Omitted fields clear the corresponding catalog property, and rotation is normalized to the nearest clockwise quarter turn:

```python
broadcast.set_video_properties(
    moq.VideoProperties(
        display=moq.Dimensions(width=1080, height=1920),
        rotation=90.0,
        flip=False,
    )
)
```

### Subscribing to media

```python
async for announcement in client.announced("prefix/"):
    catalog = await announcement.broadcast.catalog()
    track_name, track = next(iter(catalog.audio.items()))
    consumer = await announcement.broadcast.subscribe_media(track_name, track)

    async for frame in consumer:
        ...
```

### Cross-broadcast renditions

A catalog rendition may name a *different* broadcast: `track.broadcast` is a path relative to the
broadcast the catalog came from, so a transcode output at `live/hd` can describe a track that
actually lives in `live/source`. `decode_audio` and `decode_video` follow it for you. `subscribe_media`,
`subscribe_track`, `fetch_group`, and `fetch_media_group` take a track name rather than a rendition,
so resolve first:

```python
catalog = await announcement.broadcast.catalog()
track_name, track = next(iter(catalog.video.items()))

source = await announcement.broadcast.resolve(track.broadcast)
consumer = await source.subscribe_media(track_name, track)
```

`resolve(None)` (or an empty reference) returns the same broadcast, so it is safe to call
unconditionally. It needs an origin to fetch a sibling from, so it raises on a broadcast consumed
straight from a local producer.
`resolve` reports a sibling that exists but has not been announced yet as unroutable rather than
waiting for it, so await the referenced broadcast's announcement first if you may be racing it.

### Catalog extensions

Advertise application-specific metadata (for example a side-channel transcript track) as an untyped catalog section. The value is any JSON-serializable object; it rides alongside `video`/`audio` and reaches subscribers as `Catalog.sections`.

```python
import json

# Publish: attach a custom section.
broadcast = client.create_broadcast("my-stream")
broadcast.set_catalog_section("transcript", {"track": "transcript.json"})

# Subscribe: read it back. Sections are unknown to the base catalog, so decode the JSON yourself.
catalog = await announcement.broadcast.catalog()
if "transcript" in catalog.sections:
    info = json.loads(catalog.sections["transcript"])
```

`"video"` and `"audio"` are reserved names. Remove a section with `broadcast.remove_catalog_section("transcript")`.

### Raw tracks (no codec)

```python
# Publish
broadcast = moq.BroadcastProducer()
track = broadcast.publish_track("events")
track.write_frame(b'{"cmd": "ready"}', 0)
track.write_frame(b'{"cmd": "tick"}', 20_000)
track.finish()

# Subscribe
track = await broadcast_consumer.subscribe_track(
    "events",
    subscription=moq.Subscription(priority=10),
)
info = track.info()
track.update(moq.Subscription(priority=20, ordered=False))
async for group in track:
    async for frame in group:
        print(frame.timestamp_us, frame.payload)
```

`write_frame` on a track creates a one-frame group by default, using a microsecond raw-track timescale. Consumers receive a `Frame` from `read_frame()` or group iteration, carrying `payload` and `timestamp_us`. (Media tracks yield a `MediaFrame`, which adds the codec-derived `keyframe` flag.) Use `append_group()` for multi-frame groups (e.g., a video GOP).
Use `create_group(sequence)` for sparse or replayed groups. `finish_at(final_sequence)` declares the exclusive end while still permitting lower groups to arrive, and `abort(error_code)` terminates a track or group with an application error.
`TrackConsumer.info()` returns the publisher's track properties (timescale, cache, priority, ordering priority), and `update()` changes this subscriber's delivery preferences without resubscribing.
`ordered` controls prioritization only. When true, groups are prioritized in sequence order. Groups may always arrive out-of-order (or not at all) over the network.

### Fetching raw groups

Fetch retrieves one group by track name and group sequence without keeping a live subscription:

```python
group = await broadcast_consumer.fetch_group(
    "events",
    sequence=42,
    options=moq.FetchGroupOptions(priority=10),
)
async for frame in group:
    print(frame.timestamp_us, frame.payload)
```

A retained group resolves immediately. To serve a group that is not retained, keep a dynamic handler alive on its producer:

```python
dynamic = track.dynamic()

async for request in dynamic:
    group = request.accept()
    group.write_frame(load_archived_frame(request.sequence), request.sequence * 20_000)
    group.finish()
```

Call `request.abort(code)` when the requested group cannot be produced. Fetch is currently a single-group operation and is supported by the moq-lite 05+ FETCH wire path.

### Fetching media groups

`fetch_group` hands back raw payloads. `fetch_media_group` decodes the same group through the rendition's container, so you get timestamped frames without opening a live subscription:

```python
catalog = await broadcast_consumer.catalog()
name, audio = next(iter(catalog.audio.items()))

group = await broadcast_consumer.fetch_media_group(name, sequence=42, track=audio)
async for frame in group:
    print(frame.timestamp_us, len(frame.payload))
```

A fetched media group is finite: it ends after the group's last decoded frame, unlike the live `subscribe_media` stream. Latency-based group skipping does not apply, so you always get every frame in the group.

### Raw datagrams

Raw tracks can also send best-effort datagrams:

```python
seq = track.append_datagram(b"meter update", timestamp_us=42_000)
datagram = await track_consumer.recv_datagram()
```

A datagram is a single unreliable payload, returned as `Datagram(sequence, timestamp_us, payload)`. Payloads are capped at 1200 bytes. Datagram delivery requires a datagram-capable transport and lite-05 or newer moq-lite; IETF moq-transport, pre-lite-05, WebSocket, and TCP paths do not deliver them, and there is no stream fallback.

### JSON tracks

For JSON payloads, `publish_json_snapshot` / `subscribe_json_snapshot` (and the `_stream` pair) handle the framing for you. Values are ordinary Python objects (encoded with `json` internally). You opt into one of two modes:

- **Snapshot** (lossy): one value updated over time; a subscriber only sees the latest. Ideal for status documents and metadata. A late joiner catches up to the newest value in one step.
- **Stream** (lossless): an ordered append-log where every record is preserved. Ideal for event logs and timelines.

```python
# Snapshot: each update supersedes the last.
status = broadcast.publish_json_snapshot("status", compression=True)
status.update({"state": "live", "viewers": 42})
status.update({"state": "live", "viewers": 43})

async for value in broadcast_consumer.subscribe_json_snapshot("status", compression=True):
    print(value["viewers"])

# Stream: every record is delivered in order.
events = broadcast.publish_json_stream("events")
events.append({"event": "started"})

async for record in broadcast_consumer.subscribe_json_stream("events"):
    print(record["event"])
```

`compression` must match on the producer and subscriber. Snapshot mode also takes `delta_ratio` (0 disables merge-patch deltas, so every change is a fresh snapshot). Advertise the track with a catalog section if subscribers should discover it.

### On-demand raw tracks

Use a dynamic broadcast when subscribers should be able to request raw tracks that are not published yet:

```python
broadcast = client.create_broadcast("events")
dynamic = broadcast.dynamic()

async for request in dynamic:
    if request.name == "alerts":
        track = request.accept()
        track.write_frame(b"ready", 0)
        track.finish()
```

Missing track subscriptions are accepted while the `BroadcastDynamic` object is alive. Each one arrives as a `TrackRequest`; call `accept()` to turn it into a `TrackProducer` (or `abort(code)` to reject the subscriber).

### On-demand broadcasts

Use a dynamic origin when consumers should be able to request whole broadcasts that are not announced:

```python
origin = moq.OriginProducer(cache_capacity_bytes=256 * 1024 * 1024)
dynamic = origin.dynamic()

async for request in dynamic:
    if request.path == "events":
        broadcast = moq.BroadcastProducer()
        track = broadcast.publish_track("status")
        request.accept(broadcast)
        track.write_frame(b"ready", 0)
```

The served broadcast is not announced. It only resolves consumers that call `request_broadcast(path)`. Each request arrives as a `BroadcastRequest`; call `accept(broadcast)` to serve it, or `abort(code)` to fail the requester.

### Discovering broadcasts

```python
async for announcement in client.announced("live/"):
    print(announcement.path)
    print(announcement.broadcast.route.hops)  # relay origin ids, oldest first
    ...

# Or wait for a specific path to be announced:
broadcast = await client.announced_broadcast("live/cam1")

# Or request a path: resolves to the announced broadcast, falls back to a dynamic
# handler if the origin has one, else raises. Does not wait for a future announce.
broadcast = await client.request_broadcast("live/cam1")
```

Announcements arrive over the session after it connects, so `request_broadcast` on its own races them: right after connecting it can raise for a broadcast that is live. Await `announced_broadcast(path)` first when you know the path you want; `request_broadcast` is for a path a dynamic handler serves, or one you already know is announced.

Each broadcast carries a `Route`: `route.hops` is the chain of relay origin ids (as `list[int]`) the broadcast passed through to reach you, oldest first, and `route.cost` is the publisher's advertised preference (lower wins). The route is dynamic; `await broadcast.route_changed()` returns the current route first, then blocks for each change (e.g. an upstream failover), and returns `None` once the broadcast ends. A publisher advertises its own route with `producer.set_route(moq.Route(hops=[], cost=10))`, for example a standby transcoder that lowers its cost to 0 once it is warm.

## Examples

The repo ships [example scripts](https://github.com/moq-dev/moq/tree/main/py/moq-rs/examples) you can run end-to-end:

- `clock.py`: publishes / subscribes a clock track (one frame per second, one group per minute).
- `announced.py`: lists broadcasts under a prefix as they're announced.

## See also

- API reference: [moq-rs.readthedocs.io](https://moq-rs.readthedocs.io)
- Source: [py/moq-rs](https://github.com/moq-dev/moq/tree/main/py/moq-rs)
- README: [py/moq-rs/README.md](https://github.com/moq-dev/moq/blob/main/py/moq-rs/README.md)
- Raw bindings: [moq-ffi](https://pypi.org/project/moq-ffi/)
- The Rust crate this wraps: [moq-net](/lib/rs/crate/moq-net)
