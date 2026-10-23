---
title: Python
description: Async pub/sub for Python via the moq-rs package
---

# Python

[![PyPI](https://img.shields.io/pypi/v/moq-rs)](https://pypi.org/project/moq-rs/)

`moq-rs` on PyPI (the `moq` name was taken), imported as `moq`. It wraps the
generated `moq-ffi` bindings in asyncio: async context managers for sessions,
async iterators for announcements, groups, and frames, and no `Moq` prefixes.
Python 3.10+, with wheels for Linux x86\_64/aarch64, macOS arm64, and Windows
x64.

`moq.media` owns catalog snapshots, encoded-media importers, and container consumers.
Use `Named(name)` or `Requested(request)` as the target for `TrackProducer.audio` / `.video`.
`CatalogProducer(broadcast)` holds the broadcast weakly and owns catalog properties and sections;
its writes fail with `Error.Closed` after the broadcast closes or is released.
Share `media.Timebase(broadcast)` through `ContainerInit` when several imports use one
source timeline; omitting it keeps an independent offset. A timebase belongs to its
broadcast and does not keep it open. `ContainerProducer` takes chunks split anywhere,
so a raw byte stream needs no other importer.
`ContainerFormat.FMP4` splits each fragment into one frame per sample; `ContainerFormat.CMAF` republishes it whole.
`MediaFrame.timestamp`, media writes, and `flush` use `datetime.timedelta`.
A missing data timestamp stays untimed; the library never substitutes zero or now.
JSON consumers yield `moq.json.Timed(value, timestamp)`; a snapshot consumer yields each state in order,
and `await consumer.latest()` skips to the newest. A window consumer's value is a `WindowPush`, `WindowPop`, or `WindowSkip`. Read timestamps are rounded down to a whole microsecond.
Raw tracks with a `None` timescale are untimed on both publish and receive.
`TrackInfo(compress=True)` on a raw track sends each group DEFLATE-compressed to opt-in `moq-lite-07-wip` peers; other peers, and every reader here, get the original frames. `@moq/net` cannot read a compressed track yet.
`moq.flate.SnapshotProducer` / `StreamProducer` publish opaque payloads; their `update` / `append`
and the JSON producers accept `timestamp=timedelta(...)` (`None` for untimed).
Whole-frame media writes require caller time; container imports keep their own timeline handling.
Timed raw/data tracks require caller-supplied timestamps. Use `CatalogProducer.now()` to stamp a locally
created event on the broadcast clock; reading it fixes that clock for later media producers.
For arrival-order reads, decode each group from `TrackConsumer.recv_group()` with
`media.GroupConsumer(group, container)`; frames carry container media time as they land.

```bash
pip install moq-rs      # or: uv add moq-rs
```

```python
import asyncio, moq

async def main():
    async with moq.Client("https://cdn.moq.dev/anon") as client:
        # The filter is relative to the literal prefix; prefixes stay origin-relative.
        async for event in client.announced("live/", filter="*/camera"):
            # A restart is a new publisher run at the same path: request it again.
            if not isinstance(event, (moq.AnnounceEventStart, moq.AnnounceEventRestart)):
                continue  # AnnounceEventUpdate or AnnounceEventEnd
            announcement = event.announce
            print(announcement.captures)  # what * matched, or None for a partial overlap
            broadcast = await client.request_broadcast(announcement.prefix)
            catalog = await moq.media.catalog(broadcast)
            id, audio = next(iter(catalog.audio.items()))
            async for frame in await moq.media.TrackConsumer.subscribe(broadcast, id, audio):
                print(frame.timestamp, len(frame.payload))

asyncio.run(main())
```

```python
import asyncio, moq

async def main():
    # opus_init_bytes, payload, pts, and rgba come from your encoder or capture source.
    async with moq.Client("https://cdn.moq.dev/anon") as client:
        broadcast = client.create_broadcast("my-stream.hang")

        # Already-encoded frames: the catalog is filled from the bitstream
        audio = moq.media.TrackProducer.audio(broadcast, moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=opus_init_bytes))
        audio.write_frame(payload, moq.media.CatalogProducer(broadcast).now())
        audio.cut()   # audio has no keyframes, so this is what gives it groups

        # Or raw pixels, encoded inside the binding (VideoToolbox, Media Foundation, NVENC, openh264)
        catalog_producer = moq.media.CatalogProducer(broadcast)
        video = moq.video.Producer(
            catalog_producer,
            moq.VideoEncoderInput(format=moq.VideoPixelFormat.RGBA, width=1280, height=720, framerate=30),
            moq.VideoEncoderOutput(
                codec=moq.VideoCodec.H264,
                track="camera",
                kind=moq.VideoEncoderKind.AUTO(),  # pyright: ignore[reportArgumentType]
            ),
        )
        video.write(moq.video.Frame(timestamp=catalog_producer.now(), data=rgba))

        # Raw bytes and JSON
        events = broadcast.publish_track("events")
        events.write_frame(b'{"cmd": "ready"}')
        status = moq.json.SnapshotProducer(broadcast, broadcast.publish_track("status"), compression=True)
        status.update({"state": "live", "viewers": 42})

        broadcast.announce(moq.AnnounceRoute(epoch=moq.mint_epoch()))  # a fresh epoch per run
        audio.finish()
        video.finish()
        events.finish()
        status.finish()
        broadcast.close()

asyncio.run(main())
```

## Things to know

The rest of the [shared feature list](/lib/#what-every-binding-can-do) maps
one to one; the API reference has the names, and [Layers](/lib/layers)
shows which module holds each.

- **Context managers.** `async with` on a client or session awaits a graceful shutdown when the body exits cleanly, and cancels at once when it raises, so your exception survives.
- **Audio needs cuts.** Video groups at its keyframes, but audio forms a group only where you call `cut()`: after every frame, or at a segment cadence to align with video.
- **Live encoder timing.** After writing a frame you encoded yourself, call `flush(timestamp)` with the same timestamp so the catalog advertises your jitter. Skip it for file and network imports. On a seek or pause, call `discontinuity()`, then keep timestamps moving forward and resume video on a keyframe.
- **Decoded frames hold decoder buffers.** Drop each frame from a `moq.video.Consumer` promptly, or the decoder stalls. `resize` is best effort, so read each frame's `width()` and `height()`.
- **Closing.** `await session.shutdown()` drains for up to one second and raises on failure. `cancel(code)` closes at once. Finish or abort live tracks first. Application delivery requires explicitly selecting `moq-lite-07-wip` on both peers; older Lite versions (including default lite-06) and IETF wait only for transport ACKs.
- **Ending cleanly.** When the input ends, finish every track, then call `broadcast.finish()`. The returned `BroadcastFinish` keeps the broadcast announced and serving the finished tracks for `linger`, their longest max age, so a subscriber still catching up can drain them. `await finish.wait()` returns once the linger passes; cancelling it, or `finish.close()`, closes the broadcast at once. `broadcast.close()` stays the abrupt end.
- **Stats.** `session.stats()` reports `rtt`, `estimated_send_rate_bps`, and the byte and packet counters (`bytes_sent`, `bytes_received`, `bytes_lost`, `packets_sent`, `packets_received`, `packets_lost`). A field is `None` when the transport does not report it, which is not the same as zero. `version` names the negotiated protocol, and while a client session reconnects `session.failure()` says why. Before the first connect, `client.failure()` says why the dial is retrying. Hold the owned `Bandwidth.Consumer` returned by `session.recv_bandwidth()` to receive bandwidth estimates. `rate()` reads the current estimate in bits per second and `rate_changed()` waits for a new one. Release the consumer or call `cancel()` when finished; releasing the last consumer stops PROBE; the session and stats snapshots do not request it. Client consumers survive reconnects and report no estimate while disconnected.
- **Authorization.** On `moq-lite-07-wip` (opt in with `versions=`) and on moq-transport draft-17+ with the [MoQ Auth extension](/draft/moq-auth), `session.auth().grant()` is the `Grant` the relay sent back, `None` until it answers and while a client session reconnects; `await auth.grant_changed()` waits for the next one. `await auth.add(token)` presents a fresh token without reconnecting, returns an `AuthToken` whose grant joins the union until `cancel()`, and is presented again on every reconnect. A refusal raises with the relay's code.
- **Errors.** Read diagnostic payloads from `reason` and structured protocol failures from `details`.
- **Disabled renditions.** Audio and video convenience decoders reject a catalog configuration with `enabled: false`. Select an enabled rendition from the latest catalog before opening a decoder. Among the video renditions you can decode, prefer the highest `preference`; `0` is the default and `-1` marks a compatibility transcode.
- **Track demand.** Obtain a dynamic track request’s `demand()` before accepting it to watch subscribers through the handoff to its producer. The watch does not keep the track alive and ends when the track closes or the request is rejected.

## Runtime

Native I/O and background tasks run on a Tokio worker pool. Calls awaited by the
host still poll in place; independent native wake callbacks can run concurrently.
The generated bindings wake Python futures through the asyncio event loop. The
package exit hook joins native workers before interpreter finalization.
A panic in a native background task reaches the pending and later fallible calls
of every object that depends on it as a task error; other objects keep running.

A session closed for exceeding the relay's request caps reports the protocol
kind `ProtocolKind.TOO_MANY_REQUESTS`. Reduce
the simultaneous announcements or subscriptions, or ask the relay operator
to adjust the [session limits](/bin/relay/config#session-and-peer), before
reconnecting with the same workload.
A session closed because its credential expired reports `ProtocolKind.EXPIRED`, and a request refused for an expired token raises `Error.Expired`; refresh the token before retrying.
A catalog that needs a newer moq-hang draft than this build reads raises `Error.UnsupportedCatalog`; upgrade the library to read it.

## Reference

- API source: [py/moq-rs/moq](https://github.com/moq-dev/moq/tree/main/py/moq-rs/moq)
- Source and examples: [`py/moq-rs`](https://github.com/moq-dev/moq/tree/main/py/moq-rs)
- Raw bindings: [`moq-ffi`](https://pypi.org/project/moq-ffi/) on PyPI, for the unwrapped API
