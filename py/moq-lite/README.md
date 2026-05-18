# moq-lite

Ergonomic Python wrapper for [MoQ (Media over QUIC)](https://github.com/moq-dev/moq) — a next-generation live media delivery protocol providing real-time latency at massive scale.

`moq-lite` wraps the auto-generated [moq-ffi](https://pypi.org/project/moq-ffi/) bindings with a Pythonic API: no `Moq` prefixes, async iterators, context managers, and simplified connection setup.

## Installation

```bash
pip install moq-lite
```

## Quick Start

### Subscribe to a stream

```python
import asyncio
import moq_lite as moq

async def main():
    async with moq.Client("https://relay.quic.video") as client:
        async for announcement in client.announced():
            catalog = await announcement.broadcast.catalog()

            for name in catalog.audio:
                async for frame in announcement.broadcast.subscribe_media(name):
                    print(f"Got frame: {len(frame.payload)} bytes, ts={frame.timestamp_us}")

asyncio.run(main())
```

### Publish a stream

```python
import asyncio
import moq_lite as moq

async def main():
    async with moq.Client("https://relay.quic.video") as client:
        broadcast = moq.BroadcastProducer()

        # Publish an Opus audio track (init bytes from your encoder)
        audio = broadcast.publish_media("opus", opus_init_bytes)
        client.publish("my-stream", broadcast)

        # Write frames
        audio.write_frame(payload, timestamp_us=0)
        audio.write_frame(payload, timestamp_us=20000)

        # Clean up
        audio.finish()
        broadcast.finish()

asyncio.run(main())
```

### Host a server

```python
import asyncio
import moq_lite as moq

async def main():
    async with moq.Server("127.0.0.1:4443", tls_generate=["localhost"]) as server:
        broadcast = moq.BroadcastProducer()
        track = broadcast.publish_track("events")
        server.publish("hello", broadcast)
        print(f"listening on https://{server.local_addr}")

        sessions = []
        async for request in server:
            print(f"  + {request.transport} from {request.url}")
            sessions.append(await request.ok())

asyncio.run(main())
```

Reject a request instead of accepting it with `await request.close(403)`.

### Advanced: Manual origin wiring

For full control over the origin topology:

```python
import moq_lite as moq

origin = moq.OriginProducer()
client = moq.Client(
    "https://relay.quic.video",
    publish=origin,
    subscribe=origin,
)
```

## API

### Connection

- **`Client(url, *, tls_verify=True, bind=None, publish=None, subscribe=None)`** — async context manager for connecting to a relay
- **`Server(bind="[::]:443", *, tls_cert=(), tls_key=(), tls_generate=(), publish=None, subscribe=None)`** — async context manager + async iterator of incoming `Request`s
  - `.local_addr` — the bound address (useful when binding to port `0`)
  - `.publish(path, broadcast)` — publish a broadcast to be served
- **`Request`** — an incoming session, yielded by `async for request in server`
  - `.url`, `.transport` — properties
  - `.set_publish(origin)`, `.set_consume(origin)` — per-request overrides
  - `await .ok()` — complete the handshake, returns a session (hold it to keep the connection alive)
  - `await .close(code)` — reject with an HTTP status code

### Publishing

- **`BroadcastProducer()`** — create a broadcast to publish tracks into
  - `.publish_media(format, init) → MediaProducer`
  - `.finish()`
- **`MediaProducer`** — write frames to a track
  - `.write_frame(payload, timestamp_us)`
  - `.finish()`

### Subscribing

- **`BroadcastConsumer`** — subscribe to tracks within a broadcast
  - `.subscribe_catalog() → CatalogConsumer`
  - `.subscribe_media(name, max_latency_ms=10000) → MediaConsumer`
  - `await .catalog() → Catalog` (convenience)
- **`CatalogConsumer`** — async iterator of `Catalog`
- **`MediaConsumer`** — async iterator of `Frame`

### Origin (advanced)

- **`OriginProducer()`** — manage broadcast announcements
  - `.consume() → OriginConsumer`
  - `.publish(path, broadcast)`
- **`OriginConsumer`** — discover broadcasts
  - `.announced(prefix) → Announced` (async iterator)
  - `.announced_broadcast(path) → AnnouncedBroadcast` (awaitable)

### Types

- **`Catalog`** — `.audio: dict[str, Audio]`, `.video: dict[str, Video]`, `.display`, `.rotation`, `.flip`
- **`Frame`** — `.payload: bytes`, `.timestamp_us: int`, `.keyframe: bool`
- **`Audio`** — `.codec`, `.sample_rate`, `.channel_count`, `.bitrate`, `.description`
- **`Video`** — `.codec`, `.coded: Dimensions`, `.display_ratio`, `.bitrate`, `.framerate`, `.description`
- **`Dimensions`** — `.width: int`, `.height: int`

## See Also

- [moq-ffi](https://pypi.org/project/moq-ffi/) — raw UniFFI bindings (lower-level)
- [MoQ project](https://github.com/moq-dev/moq) — full monorepo with Rust server, TypeScript browser lib, and more
