---
title: moq-net (Python)
description: Python pub/sub for Media over QUIC
---

# moq-net

[![PyPI](https://img.shields.io/pypi/v/moq-net)](https://pypi.org/project/moq-net/)

Async pub/sub for [Media over QUIC](/) in Python.

The underlying transport is the Rust [`moq-net`](/lib/rs/crate/moq-net) crate, exposed through UniFFI and wrapped in a Pythonic API: no `Moq` prefixes on user-facing types, async iterators for streams, async context managers for sessions.

## Install

```bash
pip install moq-net
```

Requires Python 3.10+.

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

### Publishing media

```python
broadcast = moq.BroadcastProducer()
audio = broadcast.publish_media("opus", opus_init_bytes)
client.publish("my-stream", broadcast)

audio.write_frame(payload, timestamp_us=0)
audio.finish()
broadcast.finish()
```

Supported codec formats include `opus`, `avc3`, `hev1`, `av01`, `vp09`, and others — see [`hang`](/lib/rs/crate/hang) for the full list.

### Subscribing to media

```python
async for announcement in client.announced("prefix/"):
    catalog = await announcement.broadcast.catalog()
    track_name = next(iter(catalog.audio))
    consumer = announcement.broadcast.subscribe_media(track_name, catalog.audio[track_name].container, max_latency_ms=10_000)

    async for frame in consumer:
        ...
```

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

### Discovering broadcasts

```python
async for announcement in client.announced("live/"):
    print(announcement.path)
    ...

# Or wait for a specific path:
broadcast = await client.announced_broadcast("live/cam1")
```

## Examples

The repo ships [example scripts](https://github.com/moq-dev/moq/tree/main/py/moq-net/examples) you can run end-to-end:

- `clock.py` — publishes / subscribes a clock track (one frame per second, one group per minute).
- `announced.py` — lists broadcasts under a prefix as they're announced.

## See also

- Source: [py/moq-net](https://github.com/moq-dev/moq/tree/main/py/moq-net)
- README: [py/moq-net/README.md](https://github.com/moq-dev/moq/blob/main/py/moq-net/README.md)
- The Rust crate this wraps: [moq-net](/lib/rs/crate/moq-net)
