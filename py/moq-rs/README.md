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
            # A restart is a new publisher run at the same path: request it again.
            if not isinstance(event, (moq.AnnounceEventStart, moq.AnnounceEventRestart)):
                continue  # AnnounceEventUpdate or AnnounceEventEnd
            # A route covers a prefix and carries no broadcast, so resolve the path.
            broadcast = await client.request_broadcast(event.announce.prefix)
            catalog = await moq.media.catalog(broadcast)

            for id, audio in catalog.audio.items():
                frames = await moq.media.TrackConsumer.subscribe(broadcast, id, audio)
                async with frames:
                    async for frame in frames:
                        if isinstance(frame, moq.media.MediaFrame):
                            print(f"Got frame: {len(frame.payload)} bytes, ts={frame.timestamp}")
                        else:
                            print("Media break:", frame)


asyncio.run(main())
```

### Publish a stream

```python
import asyncio
from datetime import timedelta
import moq


async def main():
    async with moq.Client("https://cdn.moq.dev/anon") as client:
        publication = client.create_broadcast("my-stream")
        broadcast = publication.broadcast

        # Publish an Opus audio track (init bytes from your encoder)
        audio = moq.media.TrackProducer.audio(
            broadcast, moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=opus_init_bytes)
        )

        # Write frames
        # Audio has no keyframes, so `cut` is what gives it group boundaries.
        audio.write_frame(payload, timestamp=timedelta(0))
        audio.cut()
        audio.write_frame(payload, timestamp=timedelta(milliseconds=20))
        audio.cut()

        # A fresh epoch per run tells viewers a restart is a new broadcast.
        publication.announce(moq.AnnounceRoute(epoch=moq.mint_epoch()))

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
        publication = server.create_broadcast("hello")
        track = publication.broadcast.publish_track("events")
        publication.announce()
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

- **`connect(url, *, bind=None, versions=(), timeout=None, race=None, resolution_delay=None, tls=None, quic=None, websocket=None, once=False, backoff=None, publish=None, consume=None)`**. Shorthand for `Client(...)`; use as `async with moq.connect(url) as client:`. The names and nesting mirror moq-ffi's `MoqClientConfig`.
- **`Client(url, *, ...)`**, taking the same arguments. Async context manager for connecting to a relay.
  - `versions`. Protocol versions to offer, most preferred first (e.g. `"moq-lite-03"`); empty offers all, which `versions()` lists.
  - `timeout`, `race`, `resolution_delay`. Bound one connect attempt and pace Happy Eyeballs.
  - `tls=ClientTls(...)`. Certificate trust and the mTLS identity:
    - `insecure`. Skip certificate verification (local development only).
    - `roots`. PEM root certificate file path(s) to trust instead of the system roots.
    - `system_roots`. Whether to trust platform roots in addition to custom roots.
    - `fingerprints`. Hex SHA-256 fingerprint(s) to pin the peer's certificate to, the native equivalent of `serverCertificateHashes`. Accepts the values a server reports via `cert_fingerprints()`, so you can trust a self-signed certificate without `insecure=True`.
    - `cert`, `key`. Paired PEM certificate chain and private key paths for mTLS.
    - `host_name`. Verify the certificate against this name (SNI) instead of the URL's host.
  - `quic=QuicConfig(...)`. QUIC tuning: `max_streams` raises the peer's inbound stream cap; `idle_timeout`, `keep_alive` (`timedelta(0)` disables pings), `gso`, `mtu_discovery`, `congestion_control` (a `CongestionControl`), and `qlog` (a trace directory when `qlog_supported()`). `keylog=True` writes TLS session secrets to the file `SSLKEYLOGFILE` names, for decrypting a capture; without `SSLKEYLOGFILE` the client refuses to build.
  - `websocket=WebSocketConfig(...)`. The fallback raced against QUIC: `enabled=False` turns it off, and `delay` is QUIC's head start.
  - `once`, `backoff`. Redial with a `Backoff` when the transport drops; `once=True` dials once.
  - `.session`. The connecting or connected `Session` (or `None` before entering / after exit).
- **`Server(bind="[::]:443", *, tls_cert=(), tls_key=(), tls_generate=(), versions=(), max_streams=None, keylog=False, publish=None, consume=None)`**. Async context manager + async iterator of incoming `Request`s.
  - `keylog`. Write TLS session secrets to the file `SSLKEYLOGFILE` names, for decrypting a capture; without `SSLKEYLOGFILE` entering the server fails.
  - `.local_addr`. The bound address (useful when binding to port `0`).
  - `.cert_fingerprints()`. SHA-256 fingerprints of the configured TLS certificates, for `serverCertificateHashes` browser cert pinning.
  - `.create_broadcast(path) → Publication`. Create an unannounced broadcast, invisible to everyone; fill `.broadcast`, then `announce()` makes it discoverable and reachable; `.broadcast.close()` ends it.
- **`Request`**. An incoming session, yielded by `async for request in server`.
  - `.url`, `.path`, `.query`, `.transport`. The query-free path is uniform across transports; the root or missing path is `""`. The encoded query may contain credentials.
  - `await .accept(*, publish=None, consume=None) → Session`. Origins are captured when accept starts; None inherits the server default and a supplied origin replaces it. Pass fresh origins for isolation, or the same origin on both sides to share it. A second response raises `AlreadyResponded`; calls after cancel raise `Cancelled`. Complete the handshake (hold the result to keep the connection alive).
  - `await .reject(code)`. Reject with an application error code; 401 and 403 map to unauthorized.
  - `.cancel()`. Cancel an in-flight `accept()`/`reject()` call.
- **`Session`**. A connection, connecting or connected. Holding it keeps the connection alive; it is also an `async with` context manager that drains on a clean exit and cancels on an error.
  - `await .closed()`. Wait until the session closes.
  - `.cancel(code)`, `session.shutdown()`. Cancel immediately, or request a drain now and await `session.closed()` for its result, including a delivery timeout after one second. Explicit `moq-lite-07-wip` waits for the peer to receive finished tails; older Lite versions, including default lite-06, wait only for transport ACKs. IETF also waits for transport ACKs.
  - `.publish() → OriginProducer`, `.consume() → OriginConsumer`. The wired origin sides.
  - `.stats() → ConnectionStats`. Snapshot RTT, bandwidth estimates, and byte/packet counters.
  - `.state()` and `await .state_changed(after)`. Read one lifecycle snapshot and wait with its revision; first-dial failures and reconnects share the same session.
  - `.allocator() → Allocator`. Divide the send estimate between encoders and app-owned tracks.
  - `.grants() → GrantConsumer`. On `moq-lite-07-wip` and on moq-transport draft-18+ with the MoQ Auth extension, `.grant()` reads the peer's `Grant` and `async for grant in session.grants()` follows it.
  - `.tokens() → Tokens | None`. A client session's credentials: `.set([...])` replaces them without reconnecting and `await .answered()` raises the relay's refusal.

### Publishing

- **`BroadcastProducer()`**. Create a broadcast to publish tracks into.
  - `.requests() → BroadcastRequests`
  - `moq.video.Producer(media.CatalogProducer(broadcast), input, output, *, track=None, bandwidth=None)`. Encode raw video frames inside the binding.
  - `moq.audio.Producer(media.CatalogProducer(broadcast), input, output, *, track=None, bandwidth=None)`. Encode raw PCM frames. A zero frame duration uses the codec's own frame.
  - `.close()` ends the broadcast for good; a second call is a no-op.
- **`BroadcastRequests`**. Async source of tracks requested by subscribers.
  - `await .requested_track() → TrackRequest`. Call `.accept()` on it for a `TrackProducer`, or `.abort(code)` to reject.
  - Async iterator yielding `TrackRequest`
- **`TrackProducer` / `GroupProducer`**. Write raw payloads with no codec parsing.
  - `.write_frame(payload, timestamp=None)` writes a payload with optional capture time.
  - `.create_group(sequence)` creates a sparse or replayed group at an explicit sequence.
  - `.finish()` ends at the live edge; the handle remains so `.abort(error_code)` can still run.
  - `.finish_at(final_sequence)` declares the first group that will never be produced while leaving lower groups writable.
  - `.abort(error_code)` terminates the track or group with an application error.
  - `.append_datagram(payload, timestamp=None) -> sequence` (`TrackProducer`) sends a best-effort datagram. Payloads are capped at 1200 bytes and there is no stream fallback.

### Media

`moq.media` owns catalogs, encoded-media importers, and container consumers.

- `media.TrackProducer.audio(broadcast, AudioInit(...), *, target=Named())` and
  `.video(broadcast, VideoInit(...), *, target=Named())` import complete encoded frames.
  A `Named(name)` target chooses a name; `Named()` derives a unique name from the format.
  `Requested(request)` takes over a pending subscriber request, whose name is already fixed.
- `media.TrackStreamProducer.video(...)` infers video frame boundaries from a byte stream.
- `media.ContainerProducer(broadcast, ContainerInit(...))` demuxes a container from chunks split anywhere,
  so it also takes a raw byte stream.
  Share `media.Timebase(broadcast)` in the init records for containers on one source timeline;
  omitting it gives each container an independent offset. Timebases belong to one broadcast
  and do not keep it open.
- `media.CatalogProducer(broadcast)` owns `set_video_properties`, `set_section`, and
  `remove_section`. It holds the broadcast weakly; writes fail with `Error.Closed` after closing
  or releasing the broadcast. `.now()` returns its current media time as `timedelta` and fixes
  the clock for subsequent producers.
- `await media.CatalogConsumer.subscribe(broadcast)` streams catalog snapshots;
  `await media.catalog(broadcast)` reads one snapshot and releases its subscription.
- `await media.TrackConsumer.subscribe(broadcast, id, rendition, *, subscription=None)`
  decodes live media from a catalog rendition: its ID (catalog key) and its `Video` or `Audio`
  config. It follows the rendition's `broadcast` and `track` itself and yields `MediaFrame`,
  `Gap`, or `End`. An encoder end is not iterator EOF; media can resume after it.
- `await media.GroupConsumer.fetch(broadcast, id, rendition, sequence, *, options=None)`
  fetches and decodes exactly one group of a catalog rendition.
- `media.GroupConsumer(group, container)` decodes a raw group, such as one from
  `TrackConsumer.recv_group()` for arrival-order reads, with container media time on each frame
  as it lands.

`media.TrackProducer.write_frame(payload, timestamp)` requires the caller's capture time as
`timedelta`; returned `media.MediaFrame.timestamp` uses it too. `demand()` owns the track name and
subscriber waits. `.cut()` / `.seek(sequence)` draw group boundaries; `.finish()` ends the import.

### Data

`moq.json.SnapshotProducer.update(value, timestamp=None)`,
`moq.json.StreamProducer.append(value, timestamp=None)`, and
`moq.json.WindowProducer.push(value, timestamp=None)` / `.pop(count, timestamp=None)` preserve
optional capture time. A window keeps a sliding run of records a reader can join at any point,
and `.cut()` ends its open group so the next edit opens one restating the window; the catalog has
no window mode, so `WindowProducer` takes only the track.
`moq.flate.SnapshotProducer` and `moq.flate.StreamProducer` offer the same operations for bytes.
Omitting time leaves data untimed; choose the track's `timescale` to match.

### Subscribing

- **`BroadcastConsumer`**. Subscribe to tracks within a broadcast.
  - `await .subscribe_track(name, subscription=None) → TrackConsumer`
- **`TrackConsumer`**. Async iterator of raw groups, in sequence order.
  - `await .next_group() → GroupConsumer | None`. Sequence order; what the default iteration yields.
  - `await .recv_group() → GroupConsumer | None`. Arrival order, which may be out of sequence. Prefer it when latency matters more than order.
  - `.groups_as_arrived()`. Async iterator over `recv_group()`.
  - `.read_frame() -> Frame | None` returns the first frame of the next group. Empty groups are skipped; `None` is track EOF.
  - `await .recv_datagram() -> Datagram | None` for best-effort raw track datagrams.
  - `.info() → TrackInfo`
  - `.update(subscription)`. Change delivery priority, staleness, or group range after subscribing.
- **`GroupConsumer`**. Async iterator of raw `Frame`s.
  - `.read_frame() -> Frame | None` returns a raw frame with optional capture time.

Every handle whose cleanup is `cancel()` is an async context manager, so exiting `async with` releases it: the consumers (`media.CatalogConsumer`, `media.TrackConsumer`, `media.GroupConsumer`, `TrackConsumer`, `audio.Consumer`, `video.Consumer`, `GroupConsumer`, `json.SnapshotConsumer`, `json.StreamConsumer`, `json.WindowConsumer`, `AnnounceConsumer`, `AnnouncedBroadcast`) and the dynamic sources (`OriginRequests`, `BroadcastRequests`, `TrackRequests`).

### Origin (advanced)

- **`OriginProducer(*, cache_capacity_bytes=None)`**. Manage broadcast announcements. Set `cache_capacity_bytes` to bound cached groups under this origin.
  - `.consume() → OriginConsumer`
  - `.requests(prefix) → OriginRequests`
  - `.create_broadcast(path) → Publication`
- **`Publication`**. A broadcast published on an origin. Dropping it keeps the route; closing the broadcast ends it.
  - `.broadcast → BroadcastProducer`. The broadcast to fill.
  - `.announce(route=None)`, `.unannounce()`. Advertise or retract its exact path; both raise `Closed` once the broadcast closed.
- **`OriginRequests`**. Async source of broadcasts requested by consumers.
  - `await .requested_broadcast() → BroadcastRequest`. Call `.prepare(mint_epoch())`, fill its `.broadcast`, then `.publish() → Publication` to serve it under a new epoch, or `.reject(code)` to fail the requester.
  - Async iterator yielding `BroadcastRequest`
- **`OriginConsumer`**. Discover broadcasts.
  - `.announced(prefix, filter=None) → AnnounceConsumer` (async iterator of `AnnounceEvent`: `AnnounceEventStart`, `AnnounceEventUpdate`, `AnnounceEventEnd`, or `AnnounceEventRestart`, each carrying an `Announce` whose `.route.epoch` names the publisher run; on a restart, request the path again); `filter` is a pattern relative to the literal prefix, while each `Announce.prefix` stays origin-relative and `.captures` reports wildcard matches
  - `.announced_broadcast(path) → AnnouncedBroadcast` (awaitable, waits until something serves the path)
  - `.request_broadcast(path) → BroadcastConsumer` (awaitable; owns discovery and resolves one instance, or raises once synced without a route)

### Types

- **`media.Catalog`**. `.audio: dict[str, Audio]`, `.video: dict[str, Video]`, `.display`, `.rotation`, `.flip`.
- **`Frame`**. `.payload: bytes`, `.timestamp: timedelta | None`. The unit of every write and every raw read; `None` means untimed when publishing or reading.
- **`media.MediaFrame`**. `.payload: bytes`, `.timestamp: timedelta`, `.keyframe: bool`. The frame variant of a media read. `keyframe` marks a group start or video keyframe; for audio it is true only at a group start.
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
- **`is_auth(err)`**. True for `Unauthorized` (HTTP 401), `Forbidden` (HTTP 403), and `Expired` (a request token expired), and for a protocol Unauthorized or Expired session close or track revocation. Retrying without new credentials won't help, so surface these rather than reconnect.
- **`protocol_error(err)`**. The structured protocol failure (session or stream scope, verbatim wire code, known kind) when the peer sent one.

## See Also

- [`moq-ffi`](https://pypi.org/project/moq-ffi/). The raw UniFFI bindings this package wraps. Use it directly only if you need the unwrapped `Moq`-prefixed API.
- [MoQ project](https://github.com/moq-dev/moq). Full monorepo with Rust server, TypeScript browser lib, and more.
