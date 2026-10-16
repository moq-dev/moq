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
Share `media.Timebase(broadcast)` through container init records when several imports
use one source timeline. `ContainerStreamProducer` takes `ContainerStreamInit`;
omitting its timebase keeps an independent offset. A timebase belongs to its broadcast
and does not keep it open.
`MediaFrame.timestamp`, media writes, and `flush` use `datetime.timedelta`.
A missing data timestamp stays untimed; the library never substitutes zero or now.
Raw tracks with a `None` timescale are untimed on both publish and receive.
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
            name, audio = next(iter(catalog.audio.items()))
            # The track is the rendition's `track` when set, otherwise its catalog key.
            track = audio.track if audio.track is not None else name
            async for frame in await moq.media.TrackConsumer.subscribe(broadcast, track, audio.container):
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
        video = broadcast.encode_video(
            moq.VideoEncoderInput(format=moq.VideoPixelFormat.RGBA, width=1280, height=720, framerate=30),
            moq.VideoEncoderOutput(
                codec=moq.VideoCodec.H264,
                track="camera",
                kind=moq.VideoEncoderKind.AUTO(),  # pyright: ignore[reportArgumentType]
            ),
        )
        video.write(moq.VideoFrame(timestamp_us=pts, data=rgba))

        # Raw bytes and JSON
        events = broadcast.publish_track("events")
        events.write_frame(b'{"cmd": "ready"}')
        status = moq.json.SnapshotProducer(broadcast, broadcast.publish_track("status"), compression=True)
        status.update({"state": "live", "viewers": 42})

        broadcast.announce(moq.Route(epoch=moq.mint_epoch()))  # a fresh epoch per run
        audio.finish()
        video.finish()
        events.finish()
        status.finish()
        broadcast.close()

asyncio.run(main())
```

## Things to know

The rest of the [shared feature list](/lib/#what-every-binding-can-do) maps
one to one; the API reference has the names.

- **Context managers.** `async with` on a client or session awaits a graceful shutdown when the body exits cleanly, and cancels at once when it raises, so your exception survives.
- **Audio needs cuts.** Video groups at its keyframes, but audio forms a group only where you call `cut()`: after every frame, or at a segment cadence to align with video.
- **Live encoder timing.** After writing a frame you encoded yourself, call `flush(timestamp)` with the same timestamp so the catalog advertises your jitter. Skip it for file and network imports. On a seek or pause, call `discontinuity()`, then keep timestamps moving forward and resume video on a keyframe.
- **Decoded frames hold decoder buffers.** Drop each frame from `decode_video` promptly, or the decoder stalls. `resize` is best effort, so read each frame's `width()` and `height()`.
- **Closing.** `await session.shutdown()` drains for up to one second and raises on failure. `cancel(code)` closes at once. Finish or abort live tracks first. Application delivery requires explicitly selecting `moq-lite-07-wip` on both peers; older Lite versions (including default lite-06) and IETF wait only for transport ACKs.
- **Stats.** `session.stats()` reports `rtt`, `estimated_send_rate_bps`, `estimated_recv_rate_bps`, and the byte and packet counters (`bytes_sent`, `bytes_received`, `bytes_lost`, `packets_sent`, `packets_received`, `packets_lost`). A field is `None` when the transport does not report it, which is not the same as zero.
- **Errors.** Read diagnostic payloads from `reason` and structured protocol failures from `details`.
- **Disabled renditions.** Audio and video convenience decoders reject a catalog configuration with `enabled: false`. Select an enabled rendition from the latest catalog before opening a decoder.
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

## Reference

- API reference: [moq-rs.readthedocs.io](https://moq-rs.readthedocs.io)
- Source and examples: [`py/moq-rs`](https://github.com/moq-dev/moq/tree/main/py/moq-rs)
- Raw bindings: [`moq-ffi`](https://pypi.org/project/moq-ffi/) on PyPI, for the unwrapped API
