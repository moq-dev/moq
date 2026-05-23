# moq-rs

Python bindings for the [Media over QUIC](https://github.com/moq-dev/moq) Rust crates: real-time pub/sub with built-in caching, fan-out, and prioritization, on top of QUIC.

`moq-rs` wraps the auto-generated [moq-ffi](https://crates.io/crates/moq-ffi) UniFFI bindings with a Pythonic API: no `Moq` prefixes, async iterators, context managers, and simplified connection setup. At session setup it negotiates either the `moq-lite` or `moq-transport` wire protocol.

## Installation

```bash
pip install moq-rs
```

The distribution is `moq-rs`; the import name is `moq`.

## Quick Start

### Subscribe to a stream

```python
import asyncio
import moq

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
import moq

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
import moq

origin = moq.OriginProducer()
client = moq.Client(
    "https://relay.quic.video",
    publish=origin,
    subscribe=origin,
)
```

## API

### Connection

- **`Client(url, *, tls_verify=True, publish=None, subscribe=None)`**. Async context manager for connecting to a relay.

### Publishing

- **`BroadcastProducer()`**. Create a broadcast to publish tracks into.
  - `.publish_media(format, init) → MediaProducer`
  - `.finish()`
- **`MediaProducer`**. Write frames to a track.
  - `.write_frame(payload, timestamp_us)`
  - `.finish()`

### Subscribing

- **`BroadcastConsumer`**. Subscribe to tracks within a broadcast.
  - `.subscribe_catalog() → CatalogConsumer`
  - `.subscribe_media(name, max_latency_ms=10000) → MediaConsumer`
  - `await .catalog() → Catalog` (convenience)
- **`CatalogConsumer`**. Async iterator of `Catalog`.
- **`MediaConsumer`**. Async iterator of `Frame`.

### Origin (advanced)

- **`OriginProducer()`**. Manage broadcast announcements.
  - `.consume() → OriginConsumer`
  - `.publish(path, broadcast)`
- **`OriginConsumer`**. Discover broadcasts.
  - `.announced(prefix) → Announced` (async iterator)
  - `.announced_broadcast(path) → AnnouncedBroadcast` (awaitable)

### Types

- **`Catalog`**. `.audio: dict[str, Audio]`, `.video: dict[str, Video]`, `.display`, `.rotation`, `.flip`.
- **`Frame`**. `.payload: bytes`, `.timestamp_us: int`, `.keyframe: bool`.
- **`Audio`**. `.codec`, `.sample_rate`, `.channel_count`, `.bitrate`, `.description`.
- **`Video`**. `.codec`, `.coded: Dimensions`, `.display_ratio`, `.bitrate`, `.framerate`, `.description`.
- **`Dimensions`**. `.width: int`, `.height: int`.

## See Also

- [moq-ffi](https://crates.io/crates/moq-ffi). The Rust crate that produces the UniFFI bindings vendored as `moq._uniffi`.
- [MoQ project](https://github.com/moq-dev/moq). Full monorepo with Rust server, TypeScript browser lib, and more.
