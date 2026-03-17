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

- **`Client(url, *, tls_verify=True, publish=None, subscribe=None)`** — async context manager for connecting to a relay

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
