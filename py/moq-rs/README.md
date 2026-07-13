# moq

Python bindings for [Media over QUIC](https://github.com/moq-dev/moq): real-time pub/sub with built-in caching, fan-out, and prioritization, on top of QUIC.

`moq` wraps the auto-generated [`moq-ffi`](https://pypi.org/project/moq-ffi/) UniFFI bindings with a Pythonic API: no `Moq` prefixes, async iterators, context managers, and simplified connection setup. At session setup it negotiates either the `moq-lite` or `moq-transport` wire protocol.

## Installation

```bash
pip install moq
```

This pulls in the `moq-ffi` native bindings automatically. `moq` is pure Python and is versioned independently of `moq-ffi`; it floats to the latest compatible `moq-ffi` patch.

## Quick Start

### Subscribe to a stream

```python
import asyncio
import moq

async def main():
    async with moq.connect("https://relay.quic.video") as client:
        async for announcement in client.announced():
            catalog = await announcement.broadcast.catalog()

            for name, track in catalog.audio.items():
                frames = await announcement.broadcast.subscribe_media(name, track)
                async with frames:
                    async for frame in frames:
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
        client.announce("my-stream", broadcast)

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
import moq

async def main():
    async with moq.Server("127.0.0.1:4443", tls_generate=["localhost"]) as server:
        broadcast = moq.BroadcastProducer()
        track = broadcast.publish_track("events")
        server.announce("hello", broadcast)
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

- **`connect(url, *, tls_verify=True, tls_roots=None, tls_fingerprints=None, bind=None, publish=None, subscribe=None)`**. Shorthand for `Client(...)`; use as `async with moq.connect(url) as client:`.
- **`Client(url, *, tls_verify=True, tls_roots=None, tls_fingerprints=None, bind=None, publish=None, subscribe=None)`**. Async context manager for connecting to a relay.
  - `tls_roots`. PEM root certificate file path(s) to trust instead of the system roots.
  - `tls_fingerprints`. Hex SHA-256 fingerprint(s) to pin the peer's certificate to, the native equivalent of `serverCertificateHashes`. Accepts the values a server reports via `cert_fingerprints()`, so you can trust a self-signed certificate without `tls_verify=False`.
  - `.session`. The established `Session` (or `None` before connecting / after exit).
- **`Server(bind="[::]:443", *, tls_cert=(), tls_key=(), tls_generate=(), publish=None, subscribe=None)`**. Async context manager + async iterator of incoming `Request`s.
  - `.local_addr`. The bound address (useful when binding to port `0`).
  - `.cert_fingerprints()`. SHA-256 fingerprints of the configured TLS certificates, for `serverCertificateHashes` browser cert pinning.
  - `.announce(path, broadcast)`. Advertise a broadcast to be served.
- **`Request`**. An incoming session, yielded by `async for request in server`.
  - `.url`, `.transport`. Properties.
  - `.set_publish(origin)`, `.set_consume(origin)`. Per-request overrides.
  - `await .ok() → Session`. Complete the handshake (hold the result to keep the connection alive).
  - `await .close(code)`. Reject with an HTTP status code.
  - `.cancel()`. Cancel an in-flight `ok()`/`close()` call.
- **`Session`**. An established connection. Holding it keeps the connection alive; it is also an `async with` context manager that shuts down on exit.
  - `await .closed()`. Wait until the session closes.
  - `.cancel(code)`, `.shutdown()`. Close with an error code, or gracefully (code 0).
  - `.publisher() → OriginProducer`, `.consumer() → OriginConsumer`. The wired origin sides.
  - `.stats() → ConnectionStats`. Snapshot RTT, bandwidth estimates, and byte/packet counters.

### Publishing

- **`BroadcastProducer()`**. Create a broadcast to publish tracks into.
  - `.dynamic() → BroadcastDynamic`
  - `.publish_media(format, init=b"", video=None) → MediaProducer`. Pass a `VideoHint` to pin catalog fields the stream can't reveal (bitrate) or publish the catalog before the first keyframe; audio formats resolve from their init bytes.
  - `.finish()`
- **`BroadcastDynamic`**. Async source of tracks requested by subscribers.
  - `await .requested_track() → TrackRequest`. Call `.accept()` on it for a `TrackProducer`, or `.abort(code)` to reject.
  - Async iterator yielding `TrackRequest`
- **`MediaProducer`**. Write frames to a track.
  - `.write_frame(payload, timestamp_us)`
  - `.finish()`
- **`TrackProducer` / `GroupProducer`**. Write raw payloads with no codec parsing.
  - `.write_frame(payload, timestamp_us)` writes a payload with a presentation timestamp in microseconds.
  - `.append_datagram(timestamp_us, payload) -> sequence` (`TrackProducer`) sends a best-effort datagram. Payloads are capped at 1200 bytes and there is no stream fallback.

### Subscribing

- **`BroadcastConsumer`**. Subscribe to tracks within a broadcast.
  - `await .subscribe_catalog() → CatalogConsumer`
  - `await .subscribe_track(name, subscription=None) → TrackConsumer`
  - `await .subscribe_media(name, track, max_latency_ms=10000, subscription=None) → MediaConsumer`. `track` is the catalog record (e.g. `catalog.video[name]`); its container tells the decoder how to parse the bitstream.
  - `await .catalog() → Catalog` (convenience)
- **`CatalogConsumer`**. Async iterator of `Catalog`.
- **`MediaConsumer`**. Async iterator of `Frame`.
- **`TrackConsumer`**. Async iterator of groups, plus `.recv_datagram() -> Datagram | None` for best-effort raw track datagrams.
- **`TrackConsumer`**. Async iterator of raw groups.
  - `.read_frame() -> Frame | None` returns a timestamped raw frame.
  - `await .info() → TrackInfo`
  - `.update(subscription)`. Change delivery priority, group ordering priority, staleness, or group range after subscribing.
- **`GroupConsumer`**. Async iterator of timestamped `Frame`s.
  - `.read_frame() -> Frame | None` returns a timestamped raw frame.

All consumers (`CatalogConsumer`, `MediaConsumer`, `TrackConsumer`, `AudioConsumer`, `GroupConsumer`) are async context managers; exiting `async with` cancels the subscription.

### Origin (advanced)

- **`OriginProducer(cache_capacity_bytes=None)`**. Manage broadcast announcements. Set `cache_capacity_bytes` to bound cached groups under this origin.
  - `.consume() → OriginConsumer`
  - `.dynamic() → OriginDynamic`
  - `.announce(path, broadcast)`
- **`OriginDynamic`**. Async source of broadcasts requested by consumers.
  - `await .requested_broadcast() → BroadcastRequest`. Call `.accept(broadcast)` to serve it, or `.abort(code)` to fail the requester.
  - Async iterator yielding `BroadcastRequest`
- **`OriginConsumer`**. Discover broadcasts.
  - `.announced(prefix) → Announced` (async iterator)
  - `.announced_broadcast(path) → AnnouncedBroadcast` (awaitable, waits for a future announcement)
  - `.request_broadcast(path) → BroadcastConsumer` (awaitable; announced now or a dynamic fallback, else raises)

### Types

- **`Catalog`**. `.audio: dict[str, Audio]`, `.video: dict[str, Video]`, `.display`, `.rotation`, `.flip`.
- **`Frame`**. `.payload: bytes`, `.timestamp_us: int`, `.keyframe: bool`.
- **`Datagram`**. `.sequence: int`, `.timestamp_us: int`, `.payload: bytes`. Delivered only on datagram-capable transports and lite-05 or newer moq-lite.
- **`Audio`**. `.codec`, `.sample_rate`, `.channel_count`, `.bitrate`, `.description`.
- **`Video`**. `.codec`, `.coded: Dimensions`, `.display_aspect`, `.bitrate`, `.framerate`, `.description`.
- **`Subscription`**. Subscriber delivery preferences: priority, ordering priority, staleness, and optional group range.
- **`TrackInfo`**. Publisher track properties: priority, ordering priority, cache window, and timescale.
- **`Dimensions`**. `.width: int`, `.height: int`.
- **`Container`**. The catalog container enum, carried on each `Video`/`Audio` record.

For both `Subscription` and `TrackInfo`, `ordered` controls prioritization only. When true, groups are prioritized in sequence order. Groups may always arrive out-of-order (or not at all) over the network.

### Logging and errors

- **`log_level(level="info")`**. Initialize logging for the underlying Rust layer (`"error"`, `"warn"`, `"info"`, `"debug"`, `"trace"`). Call once per process.
- **`Error`**. The exception raised by all operations. Catch a specific case via its variants, e.g. `except moq.Error.AlreadyResponded:` or `except moq.Error.Cancelled:`.

## See Also

- [`moq-ffi`](https://pypi.org/project/moq-ffi/). The raw UniFFI bindings this package wraps. Use it directly only if you need the unwrapped `Moq`-prefixed API.
- [MoQ project](https://github.com/moq-dev/moq). Full monorepo with Rust server, TypeScript browser lib, and more.
