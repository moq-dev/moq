---
title: moq-rs (Python)
description: Python pub/sub for Media over QUIC
---

# moq-rs

[![PyPI](https://img.shields.io/pypi/v/moq-rs)](https://pypi.org/project/moq-rs/)

Async pub/sub for [Media over QUIC](/) in Python.

The underlying transport is the Rust [`moq-net`](/lib/rs/crate/moq-net) crate, exposed through UniFFI (the [`moq-ffi`](https://pypi.org/project/moq-ffi/) package) and wrapped in a Pythonic API: no `Moq` prefixes on user-facing types, async iterators for streams, async context managers for sessions. `moq-rs` is versioned independently of `moq-ffi` and floats to the latest compatible patch.

## Install

```bash
pip install moq-rs
```

Requires Python 3.10+. The distribution is `moq-rs` (the `moq` name is taken on PyPI); the import name is `moq`. Installing it pulls in the `moq-ffi` native bindings automatically.

## Concepts

A **broadcast** is a collection of tracks identified by a path. A **track** is a live stream of frames. Producers write broadcasts to an origin; consumers subscribe to whatever has been announced.

For unstructured byte streams (status, commands, sensor data), use `publish_track` / `subscribe_track`. For media with a known container format (audio/video), use `publish_media` / `subscribe_media` and the catalog will be populated automatically.

## API summary

### Connection

```python
async with moq.Client("https://relay.example.com") as client:
    ...
```

`Client(url, *, tls_verify=True, publish=None, subscribe=None)`. Without `publish` / `subscribe` an internal origin is created automatically. Pass an `OriginProducer` to share state across multiple clients.

A server can reject the connection on auth grounds: `moq.MoqError.Unauthorized` (HTTP 401) or `moq.MoqError.Forbidden` (HTTP 403). These are terminal, so handle them separately from a transient transport failure rather than reconnecting:

```python
try:
    async with moq.Client("https://relay.example.com") as client:
        ...
except (moq.MoqError.Unauthorized, moq.MoqError.Forbidden):
    ...  # Prompt for credentials; don't reconnect.
```

### Publishing media

```python
broadcast = moq.BroadcastProducer()
audio = broadcast.publish_media("opus", opus_init_bytes)
client.publish("my-stream", broadcast)

audio.write_frame(payload, timestamp_us=0)
audio.finish()
broadcast.finish()
```

Supported codec formats include `opus`, `avc3`, `hev1`, `av01`, `vp09`, and others. See [`hang`](/lib/rs/crate/hang) for the full list.

### Subscribing to media

```python
async for announcement in client.announced("prefix/"):
    catalog = await announcement.broadcast.catalog()
    track_name = next(iter(catalog.audio))
    consumer = announcement.broadcast.subscribe_media(track_name, catalog.audio[track_name].container, max_latency_ms=10_000)

    async for frame in consumer:
        ...
```

### Catalog extensions

Advertise application-specific metadata (for example a side-channel transcript track) as an untyped catalog section. The value is any JSON string; it rides alongside `video`/`audio` and reaches subscribers as `Catalog.extra`.

```python
import json

# Publish: attach a custom section.
broadcast = moq.BroadcastProducer()
broadcast.set_catalog_section("transcript", json.dumps({"track": "transcript.json"}))
client.publish("my-stream", broadcast)

# Subscribe: read it back. Sections are unknown to the base catalog, so decode the JSON yourself.
catalog = await announcement.broadcast.catalog()
if "transcript" in catalog.extra:
    info = json.loads(catalog.extra["transcript"])
```

`"video"` and `"audio"` are reserved names. Remove a section with `broadcast.remove_catalog_section("transcript")`.

### Raw tracks (no codec)

```python
# Publish
broadcast = moq.BroadcastProducer()
track = broadcast.publish_track("events")
track.write_frame(b'{"cmd": "ready"}')
track.finish()

# Subscribe
async for group in broadcast_consumer.subscribe_track("events"):
    async for frame in group:
        print(frame)
```

`write_frame` on a track creates a one-frame group by default. Use `append_group()` for multi-frame groups (e.g., a video GOP).

### JSON tracks

For JSON payloads, `publish_json` / `subscribe_json` handle the framing for you. Values are ordinary Python objects (encoded with `json` internally), in one of two modes:

- **Snapshot** (lossy): one value updated over time; a subscriber only sees the latest. Ideal for status documents and metadata. A late joiner catches up to the newest value in one step.
- **Stream** (lossless): an ordered append-log where every record is preserved. Ideal for event logs and timelines.

```python
# Snapshot: each update supersedes the last.
status = broadcast.publish_json("status", compression=True)
status.update({"state": "live", "viewers": 42})
status.update({"state": "live", "viewers": 43})

async for value in broadcast_consumer.subscribe_json("status", compression=True):
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
broadcast = moq.BroadcastProducer()
dynamic = broadcast.dynamic()
client.publish("events", broadcast)

async for track in dynamic:
    if track.name == "alerts":
        track.write_frame(b"ready")
        track.finish()
```

Missing track subscriptions are accepted while the `BroadcastDynamic` object is alive. Each requested track is returned as a `TrackProducer`.

### Discovering broadcasts

```python
async for announcement in client.announced("live/"):
    print(announcement.path)
    print(announcement.hops)  # relay origin ids the broadcast traversed, oldest first
    ...

# Or wait for a specific path:
broadcast = await client.announced_broadcast("live/cam1")
```

`announcement.hops` is the chain of relay origin ids (as `list[int]`) the broadcast passed through to reach you, oldest first. It is useful for routing decisions such as preferring a nearby edge or detecting how many relays a broadcast crossed.

## Examples

The repo ships [example scripts](https://github.com/moq-dev/moq/tree/main/py/moq-rs/examples) you can run end-to-end:

- `clock.py`: publishes / subscribes a clock track (one frame per second, one group per minute).
- `announced.py`: lists broadcasts under a prefix as they're announced.

## See also

- Source: [py/moq-rs](https://github.com/moq-dev/moq/tree/main/py/moq-rs)
- README: [py/moq-rs/README.md](https://github.com/moq-dev/moq/blob/main/py/moq-rs/README.md)
- Raw bindings: [moq-ffi](https://pypi.org/project/moq-ffi/)
- The Rust crate this wraps: [moq-net](/lib/rs/crate/moq-net)
