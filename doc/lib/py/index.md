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
`ContainerFormat.FMP4` splits each fragment into one frame per sample.
`MediaFrame.timestamp`, media writes, and `flush` use `datetime.timedelta`.
A missing data timestamp stays untimed; the library never substitutes zero or now.
JSON consumers yield `moq.json.Timed(value, timestamp)`; a snapshot consumer yields each state in order,
and `await consumer.latest()` skips to the newest. A window consumer's value is a `WindowPush`, `WindowPop`, or `WindowSkip`. Read timestamps are rounded down to a whole microsecond.
Raw tracks with a `None` timescale are untimed on both publish and receive.
`TrackInfo(compress=True)` sends each group DEFLATE-compressed to opt-in `moq-lite-07-wip` peers; other peers, and every reader here, get the original frames. Set it on JSON tracks.
The JSON producers' `update` / `append` accept `timestamp=timedelta(...)` (`None` for untimed).
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
            # A restart is a new publisher run at the same path: request it again. A
            # takeover continues what an earlier route served, like a start.
            if not isinstance(event, (moq.AnnounceEventStart, moq.AnnounceEventRestart, moq.AnnounceEventTakeover)):
                continue  # AnnounceEventUpdate or AnnounceEventEnd
            announcement = event.announce
            print(announcement.captures)  # what * matched
            broadcast = await client.request(announcement.prefix)
            catalog = await moq.media.catalog(broadcast)
            id, audio = next(iter(catalog.audio.items()))
            async for frame in await moq.media.TrackConsumer.subscribe(broadcast, id, audio):
                if isinstance(frame, moq.media.MediaFrame):
                    print(frame.timestamp, len(frame.payload))
                else:
                    print("media break", frame)

asyncio.run(main())
```

```python
import asyncio, moq

async def main():
    # opus_init_bytes, payload, pts, and rgba come from your encoder or capture source.
    async with moq.Client("https://cdn.moq.dev/anon") as client:
        publication = client.create_broadcast("my-stream.hang")
        broadcast = publication.broadcast

        # Already-encoded frames: the catalog is filled from the bitstream
        audio = moq.media.TrackProducer.audio(broadcast, moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=opus_init_bytes))
        audio.write_frame(payload, moq.media.CatalogProducer(broadcast).now())
        audio.cut()   # audio has no keyframes, so this is what gives it groups

        # Or raw pixels, encoded inside the binding (VideoToolbox, Media Foundation, NVENC, openh264)
        catalog_producer = moq.media.CatalogProducer(broadcast)
        video = moq.video.Producer(
            catalog_producer,
            moq.VideoEncoderInput(format=moq.VideoPixelFormat.RGBA, width=1280, height=720, framerate=moq.video.FrameRate(numerator=30000, denominator=1001)),
            moq.VideoEncoderOutput(
                codec=moq.VideoCodec.H264,
                kind=moq.VideoEncoderKind.AUTO(),  # pyright: ignore[reportArgumentType]
            ),
            track="camera",
        )
        video.write(moq.video.Frame(timestamp=catalog_producer.now(), data=rgba))

        # Raw bytes and JSON
        events = broadcast.publish_track("events")
        events.write_frame(b'{"cmd": "ready"}')
        status_track = broadcast.publish_track("status", moq.TrackInfo(compress=True))
        status = moq.json.SnapshotProducer(broadcast, status_track)
        status.update({"state": "live", "viewers": 42})

        publication.announce(moq.AnnounceRoute(epoch=moq.mint_epoch()))  # a fresh epoch per run
        audio.finish()
        video.finish()
        events.finish()
        status.finish()
        broadcast.close()

asyncio.run(main())
```

Raw video encoders take an exact numerator/denominator frame rate, such as 30000/1001. Pass the publication track name to the producer, alongside its input and codec settings. For simulcast, give each rendition the next one down as `lower`: it stops encoding, advertised as disabled, while its bandwidth grant cannot fund it, and resumes once the grant recovers. A rung only disables while the session probes for bandwidth, which on native transports takes moq-quic with BBR3.

## Things to know

The rest of the [shared feature list](/lib/#what-every-binding-can-do) maps
one to one; the API reference has the names, and [Layers](/lib/layers)
shows which module holds each.

- **Context managers.** `async with` on a client or session awaits a graceful shutdown when the body exits cleanly, and cancels at once when it raises, so your exception survives.
- **Audio needs cuts.** Video groups at its keyframes, but audio forms a group only where you call `cut()`: after every frame, or at a segment cadence to align with video.
- **Timeline breaks.** On a seek or pause, call `discontinuity()`, then keep timestamps moving forward and resume video on a keyframe.
- **Media reads.** `media.TrackConsumer`, `moq.audio.Consumer`, and `moq.video.Consumer` yield, in order, frames, a `Gap` where media was lost, and an `End` where the publisher stopped its encoder. A gap breaks continuity. An encoder end is not stream EOF: following frames before its endpoint are the old encoder's drain; the first at or past it starts fresh. A decoder reads everything the track still holds unless a live player sets `start=moq.media.DecodeStart.LATEST` in its decoder output, which joins at the newest group and skips the cached backlog.
- **Decoded frames hold decoder buffers.** Drop each frame from a `moq.video.Consumer` promptly, or the decoder stalls. `resize` is best effort, so read each frame's `width()` and `height()`.
- **Closing.** `session.shutdown()` requests a drain immediately; `await session.closed()` waits up to one second and reports its result. `cancel(code)` closes at once. Finish or abort live tracks first. Application delivery requires explicitly selecting `moq-lite-07-wip` on both peers; older Lite versions (including default lite-06) and IETF wait only for transport ACKs.
- **Ending cleanly.** When the input ends, finish every track, then call `finish()` on its `moq.media.CatalogProducer` to finish the catalog, which ends the content. The broadcast stays announced and keeps serving the finished tracks, so a subscriber still catching up can drain them, until `broadcast.close()` takes it offline. How long to keep it up is yours to decide. A track still open at the close, or released without a finish, ends with `Unroutable`, which readers take as the publisher leaving.
- **Stats.** `session.stats()` reports `rtt`, `estimated_send_rate_bps`, and the byte and packet counters (`bytes_sent`, `bytes_received`, `bytes_lost`, `packets_sent`, `packets_received`, `packets_lost`). A field is `None` when the transport does not report it, which is not the same as zero. `version` names the negotiated protocol. Connect returns a connecting session; read its coherent `state()` snapshot and wait with that snapshot's revision to observe first-dial failures and reconnects. Each watcher keeps its own revision. Hold the owned `BandwidthConsumer` returned by `session.recv_bandwidth()` to receive bandwidth estimates. `rate()` reads the current estimate in bits per second and `rate_changed()` waits for a new one. Release the consumer or call `cancel()` when finished; releasing the last consumer stops PROBE; the session and stats snapshots do not request it. Client consumers survive reconnects and report no estimate while disconnected.
- **Authorization.** On `moq-lite-07-wip` (opt in with `versions=`) and on moq-transport draft-18+ with the [MoQ Auth extension](/draft/moq-auth), `async for grant in session.grants()` yields the `Grant` the relay sent back and each change, `None` until it answers and while a client session reconnects. `grants().grant()` reads it once; cancelling or leaving an `async with` releases the wait.
- **Tokens.** Pass `tokens=[...]` instead of putting the credential in the URL. The first is the connection credential, sent in SETUP and, while an offered version can't carry it there (lite before 07), as the URL's `?jwt=`; each other token widens the grant over AUTH. `session.tokens().set([...])` replaces a client session's tokens before they expire: the next dial presents them, a live AUTH session at once, and after an authorization refusal the session redials only if they changed. `await tokens.answered()` returns once the relay accepts them and raises its refusal with the relay's code. A `?jwt=` already in the URL stays the connection credential, which is how to reach a relay older than this release.
- **Errors.** Read diagnostic payloads from `reason` and structured protocol failures from `details`.
- **Disabled renditions.** A rendition with `enabled: false` has no frames coming until its publisher re-enables it. Select an enabled rendition from the latest catalog before opening a decoder. Keep one already playing while every rendition of its kind is disabled: the convenience decoders accept a disabled configuration, and a re-enable, such as an unmute, then plays without a resubscribe. Among the video renditions you can decode, prefer the highest `preference`; `0` is the default and `-1` marks a compatibility transcode.
- **Subscription defaults.** `moq.Subscription.default()` returns moq-net's defaults, whose zero `max_delay` skips a superseded group at once; passing no subscription means the same. `dataclasses.replace` it rather than starting from `moq.Subscription()`, whose `None` `max_delay` sets no subscriber limit.
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
A session closed because its credential expired reports `ProtocolKind.EXPIRED`, and a request refused for an expired token raises `Error.Expired`; refresh the token before retrying. Malformed credentials report `ProtocolKind.MALFORMED_AUTH_TOKEN`; correct the token before reconnecting with it.
A catalog that needs a newer moq-hang draft than this build reads raises `Error.UnsupportedCatalog`; upgrade the library to read it.

## Reference

- API source: [py/moq-rs/moq](https://github.com/moq-dev/moq/tree/main/py/moq-rs/moq)
- Source and examples: [`py/moq-rs`](https://github.com/moq-dev/moq/tree/main/py/moq-rs)
- Raw bindings: [`moq-ffi`](https://pypi.org/project/moq-ffi/) on PyPI, for the unwrapped API
