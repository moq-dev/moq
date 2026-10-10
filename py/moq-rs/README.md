# moq

Python bindings for [Media over QUIC](https://github.com/moq-dev/moq): real-time pub/sub with built-in caching, fan-out, and prioritization, on top of QUIC.

Installed as [`moq-rs`](https://pypi.org/project/moq-rs/) (the `moq` name is taken on PyPI), imported as `moq`.

It wraps the auto-generated [`moq-ffi`](https://pypi.org/project/moq-ffi/) UniFFI bindings with a Pythonic API: no `Moq` prefixes, async iterators, context managers, and simplified connection setup. At session setup it negotiates either the `moq-lite` or `moq-transport` wire protocol.

## Installation

```bash
pip install moq-rs

# or with uv
uv add moq-rs
```

This pulls in the `moq-ffi` native bindings automatically. `moq-rs` is pure Python and is versioned independently of `moq-ffi`; it floats to the latest compatible `moq-ffi` patch.

## Quick Start

### Subscribe to a stream

```python
import asyncio
import moq


async def main():
    async with moq.connect("https://cdn.moq.dev/anon") as client:
        async for event in client.announced():
            if not isinstance(event, moq.AnnounceEventStart):
                continue  # AnnounceEventUpdate, AnnounceEventEnd, or AnnounceEventRestart
            # A route covers a prefix and carries no broadcast, so resolve the path.
            broadcast = await client.request_broadcast(event.announce.prefix)
            catalog = await moq.media.catalog(broadcast)

            for name, track in catalog.audio.items():
                frames = await moq.media.ContainerConsumer.subscribe(broadcast, name, track.container)
                async with frames:
                    async for frame in frames:
                        print(f"Got frame: {len(frame.payload)} bytes, ts={frame.timestamp}")


asyncio.run(main())
```

### Publish a stream

```python
import asyncio
from datetime import timedelta
import moq


async def main():
    async with moq.Client("https://cdn.moq.dev/anon") as client:
        broadcast = client.create_broadcast("my-stream")

        # Publish an Opus audio track (init bytes from your encoder)
        audio = moq.media.TrackProducer.audio(
            broadcast, moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=opus_init_bytes)
        )

        # Write frames
        # Audio has no keyframes, so `cut` is what gives it group boundaries.
        audio.write_frame(payload)
        audio.cut()
        audio.write_frame(payload, timestamp=timedelta(milliseconds=20))
        audio.cut()

        # A fresh epoch per run tells viewers a restart is a new broadcast.
        broadcast.announce(moq.Route(epoch=moq.mint_epoch()))

        # Clean up
        audio.finish()
        broadcast.close()


asyncio.run(main())
```

### Host a server

```python
import asyncio
import moq


async def main():
    async with moq.Server("127.0.0.1:4443", tls_generate=["localhost"]) as server:
        broadcast = server.create_broadcast("hello")
        track = broadcast.publish_track("events")
        broadcast.announce()
        print(f"listening on https://{server.local_addr}")

        sessions = []
        async for request in server:
            safe_path = request.path.split("?", 1)[0]
            safe_url = request.url.split("?", 1)[0] if request.url else None
            print(f"  + {request.transport} {safe_path} from {safe_url}")
            sessions.append(await request.accept())


asyncio.run(main())
```

Reject a request instead of accepting it with `await request.reject(403)`.

### Advanced: Manual origin wiring

For full control over the origin topology:

```python
import moq

origin = moq.OriginProducer()
client = moq.Client(
    "https://cdn.moq.dev/anon",
    publish=origin,
    consume=origin,
)
```

## API

### Connection

- **`connect(url, *, tls_verify=True, tls_roots=(), tls_system_roots=None, tls_fingerprints=(), tls_cert=None, tls_key=None, bind=None, versions=(), max_streams=None, websocket_enabled=None, websocket_delay=None, reconnect=True, backoff=None, publish=None, consume=None)`**. Shorthand for `Client(...)`; use as `async with moq.connect(url) as client:`.
- **`Client(url, *, ...)`**, taking the same arguments. Async context manager for connecting to a relay.
  - `tls_roots`. PEM root certificate file path(s) to trust instead of the system roots.
  - `tls_system_roots`. Whether to trust platform roots in addition to custom roots.
  - `tls_fingerprints`. Hex SHA-256 fingerprint(s) to pin the peer's certificate to, the native equivalent of `serverCertificateHashes`. Accepts the values a server reports via `cert_fingerprints()`, so you can trust a self-signed certificate without `tls_verify=False`.
  - `tls_cert`, `tls_key`. Paired PEM certificate chain and private key paths for mTLS.
  - `max_streams`. Raise the peer's inbound stream cap.
  - `versions`. Protocol versions to offer, most preferred first (e.g. `"moq-lite-03"`); empty offers all.
  - `reconnect`, `backoff`. Redial with a `Backoff` when the transport drops; `reconnect=False` dials once.
  - `.session`. The established `Session` (or `None` before connecting / after exit).
- **`Server(bind="[::]:443", *, tls_cert=(), tls_key=(), tls_generate=(), versions=(), max_streams=None, publish=None, consume=None)`**. Async context manager + async iterator of incoming `Request`s.
  - `.local_addr`. The bound address (useful when binding to port `0`).
  - `.cert_fingerprints()`. SHA-256 fingerprints of the configured TLS certificates, for `serverCertificateHashes` browser cert pinning.
  - `.create_broadcast(path) → BroadcastProducer`. Create an unannounced broadcast, invisible to everyone; `announce()` makes it discoverable and reachable; `close()` ends it.
- **`Request`**. An incoming session, yielded by `async for request in server`.
  - `.url`, `.path`, `.query`, `.transport`. The query-free path is uniform across transports; the root or missing path is `""`. The encoded query may contain credentials.
  - `await .accept(*, publish=None, consume=None) → Session`. Origins are captured when accept starts; None inherits the server default and a supplied origin replaces it. Pass fresh origins for isolation, or the same origin on both sides to share it. A second response raises `AlreadyResponded`; calls after cancel raise `Cancelled`. Complete the handshake (hold the result to keep the connection alive).
  - `await .reject(code)`. Reject with an application error code; 401 and 403 map to unauthorized.
  - `.cancel()`. Cancel an in-flight `accept()`/`reject()` call.
- **`Session`**. An established connection. Holding it keeps the connection alive; it is also an `async with` context manager that drains on a clean exit and cancels on an error.
  - `await .closed()`. Wait until the session closes.
  - `.cancel(code)`, `await session.shutdown()`. Cancel immediately, or drain finished tracks within one second, raising on failure.
  - `.publish() → OriginProducer`, `.consume() → OriginConsumer`. The wired origin sides.
  - `.stats() → ConnectionStats`. Snapshot RTT, bandwidth estimates, and byte/packet counters.
  - `await .status() → ConnectionStatus`, `.connects()`. Watch reconnects; `connects()` counts connections, 1 on the first.
  - `.bandwidth() → Bandwidth`. Divide the send estimate between encoders and app-owned tracks.

### Publishing

- **`BroadcastProducer()`**. Create a broadcast to publish tracks into.
  - `.dynamic() → BroadcastDynamic`
  - `.encode_video(input, output, *, bandwidth=None) → VideoProducer`. Encode raw `VideoFrame`s inside the binding; `.write(frame)` each one.
  - `.encode_audio(name, input, output, *, bandwidth=None) → AudioProducer`. Encode raw PCM `AudioFrame`s; the codec is `output.codec`, e.g. `AudioCodec.opus()` or `AudioCodec.aac()`, with `output.frame_duration_us` setting the Opus frame length (0 takes the codec's own frame, which AAC needs).
  - `.close()` ends the broadcast for good; a second call is a no-op.
- **`BroadcastDynamic`**. Async source of tracks requested by subscribers.
  - `await .requested_track() → TrackRequest`. Call `.accept()` on it for a `TrackProducer`, or `.abort(code)` to reject.
  - Async iterator yielding `TrackRequest`
- **`TrackProducer` / `GroupProducer`**. Write raw payloads with no codec parsing.
  - `.write_frame(payload, timestamp=timedelta(0))` writes a payload with its presentation timestamp.
  - `.create_group(sequence)` creates a sparse or replayed group at an explicit sequence.
  - `.finish()` ends at the live edge; the handle remains so `.abort(error_code)` can still run.
  - `.finish_at(final_sequence)` declares the first group that will never be produced while leaving lower groups writable.
  - `.abort(error_code)` terminates the track or group with an application error.
  - `.append_datagram(payload, timestamp=timedelta(0)) -> sequence` (`TrackProducer`) sends a best-effort datagram. Payloads are capped at 1200 bytes and there is no stream fallback.

### Media

`moq.media` owns catalogs, encoded-media importers, and container consumers.

- `media.TrackProducer.audio(broadcast, AudioInit(...), *, target=Named())` and
  `.video(broadcast, VideoInit(...), *, target=Named())` import complete encoded frames.
  A `Named(name)` target chooses a name; `Named()` derives a unique name from the format.
  `Requested(request)` takes over a pending subscriber request, whose name is already fixed.
- `media.TrackStreamProducer.video(...)` infers video frame boundaries from a byte stream.
- `media.ContainerProducer(broadcast, ContainerInit(...))` demuxes complete container chunks;
  `media.ContainerStreamProducer(broadcast, format)` recovers framing from a byte stream.
- `media.CatalogProducer(broadcast)` owns `set_video_properties`, `set_section`, and
  `remove_section`. It holds the broadcast weakly; writes fail with `Error.Closed` after closing
  or releasing the broadcast.
- `await media.CatalogConsumer.subscribe(broadcast)` streams catalog snapshots;
  `await media.catalog(broadcast)` reads one snapshot and releases its subscription.
- `await media.ContainerConsumer.subscribe(broadcast, name, container, *, subscription=None)`
  decodes live media. Pass the catalog rendition's `container`.
- `await media.ContainerGroupConsumer.fetch(broadcast, name, sequence, container, *, options=None)`
  fetches and decodes exactly one group.

`media.TrackProducer.write_frame(payload, timestamp=timedelta(0))` and `.flush(timestamp)` use
`timedelta`; returned `media.MediaFrame.timestamp` does too. `demand()` owns the track name and
subscriber waits. `.cut()` / `.seek(sequence)` draw group boundaries; `.finish()` ends the import.

### Subscribing

- **`BroadcastConsumer`**. Subscribe to tracks within a broadcast.
  - `await .subscribe_track(name, subscription=None) → TrackConsumer`
- **`TrackConsumer`**. Async iterator of raw groups, in sequence order.
  - `await .next_group() → GroupConsumer | None`. Sequence order; what the default iteration yields.
  - `await .recv_group() → GroupConsumer | None`. Arrival order, which may be out of sequence. Prefer it when latency matters more than order.
  - `.groups_as_arrived()`. Async iterator over `recv_group()`.
  - `.read_frame() -> Frame | None` returns the first timestamped frame of the next group. Empty groups are skipped; `None` is track EOF.
  - `await .recv_datagram() -> Datagram | None` for best-effort raw track datagrams.
  - `.info() → TrackInfo`
  - `.update(subscription)`. Change delivery priority, staleness, or group range after subscribing.
- **`GroupConsumer`**. Async iterator of timestamped `Frame`s.
  - `.read_frame() -> Frame | None` returns a timestamped raw frame.

Every handle whose cleanup is `cancel()` is an async context manager, so exiting `async with` releases it: the consumers (`media.CatalogConsumer`, `media.ContainerConsumer`, `media.ContainerGroupConsumer`, `TrackConsumer`, `AudioConsumer`, `GroupConsumer`, `json.SnapshotConsumer`, `json.StreamConsumer`, `AnnounceConsumer`, `AnnouncedBroadcast`) and the dynamic sources (`OriginDynamic`, `BroadcastDynamic`, `TrackDynamic`).

### Origin (advanced)

- **`OriginProducer(*, cache_capacity_bytes=None)`**. Manage broadcast announcements. Set `cache_capacity_bytes` to bound cached groups under this origin.
  - `.consume() → OriginConsumer`
  - `.dynamic(prefix, route=Route()) → OriginDynamic`
  - `.create_broadcast(path) → BroadcastProducer`
- **`OriginDynamic`**. Async source of broadcasts requested by consumers.
  - `await .requested_broadcast() → BroadcastRequest`. Call `.accept(broadcast)` to serve it, or `.reject(code)` to fail the requester.
  - Async iterator yielding `BroadcastRequest`
- **`OriginConsumer`**. Discover broadcasts.
  - `.announced(prefix, filter=None) → AnnounceConsumer` (async iterator of `AnnounceEvent`: `AnnounceEventStart`, `AnnounceEventUpdate`, `AnnounceEventEnd`, or `AnnounceEventRestart`, each carrying an `Announce` whose `.route.epoch` names the publisher run; on a restart, request the path again); `filter` is a pattern relative to the literal prefix, while each `Announce.prefix` stays origin-relative and `.captures` reports wildcard matches
  - `.announced_broadcast(path) → AnnouncedBroadcast` (awaitable, waits until something serves the path)
  - `.request_broadcast(path) → BroadcastConsumer` (awaitable; announced now or a dynamic fallback, else raises)

### Types

- **`media.Catalog`**. `.audio: dict[str, Audio]`, `.video: dict[str, Video]`, `.display`, `.rotation`, `.flip`.
- **`Frame`**. `.payload: bytes`, `.timestamp: timedelta | None`. The unit of every write and every raw read; `None` only on a frame read from an untimed track.
- **`media.MediaFrame`**. `.payload: bytes`, `.timestamp: timedelta`, `.keyframe: bool`. Returned by media subscriptions. `keyframe` marks a group start or video keyframe; for audio it is true only at a group start.
- **`Datagram`**. `.sequence: int`, `.timestamp: timedelta | None`, `.payload: bytes`. Delivered only on datagram-capable transports with lite-05 or newer moq-lite, or moq-transport.
- **`media.Audio`**. `.codec`, `.sample_rate`, `.channel_count`, `.bitrate`, `.enabled`, `.description`.
- **`media.Video`**. `.codec`, `.coded: Dimensions`, `.display_aspect`, `.bitrate`, `.enabled`, `.framerate`, `.description`. A false `.enabled` means no frames are coming, so don't select the rendition.
- **`Subscription`**. Subscriber delivery preferences: priority, staleness, and optional group range.
- **`TrackInfo`**. Publisher track properties: priority, cache window, and timescale.
- **`media.Dimensions`**. `.width: int`, `.height: int`.
- **`media.Container`**. The catalog container enum, carried on each `Video`/`Audio` record.

### Logging and errors

- **`log_level(level="info")`**. Initialize logging for the underlying Rust layer (`"error"`, `"warn"`, `"info"`, `"debug"`, `"trace"`). Call once per process.
- **`Error`**. The exception raised by all operations. Catch a specific case via its variants, e.g. `except moq.Error.AlreadyResponded:`, `except moq.Error.Cancelled:`, or `except moq.Error.Busy:` when a setter races an in-flight connect/listen/accept.
- **`is_shutdown(err)`**. True for `Cancelled` and `Closed`, which arise from graceful shutdown rather than an actual failure. Use it to break out of an `async for` without treating the expected end-of-stream error as a problem.
- **`is_auth(err)`**. True for `Unauthorized` (HTTP 401) and `Forbidden` (HTTP 403), and for a protocol Unauthorized session close. Retrying without new credentials won't help, so surface these rather than reconnect.
- **`protocol_error(err)`**. The structured protocol failure (session or stream scope, verbatim wire code, known kind) when the peer sent one.

## See Also

- [`moq-ffi`](https://pypi.org/project/moq-ffi/). The raw UniFFI bindings this package wraps. Use it directly only if you need the unwrapped `Moq`-prefixed API.
- [MoQ project](https://github.com/moq-dev/moq). Full monorepo with Rust server, TypeScript browser lib, and more.
