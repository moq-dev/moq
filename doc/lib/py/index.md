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

```bash
pip install moq-rs      # or: uv add moq-rs
```

```python
import asyncio, moq

async def main():
    async with moq.Client("https://cdn.moq.dev/anon") as client:
        # The filter is relative to the literal prefix; prefixes stay origin-relative.
        async for event in client.announced("live/", filter="*/camera"):
            if not isinstance(event, moq.AnnounceEventStart):
                continue  # AnnounceEventUpdate or AnnounceEventEnd
            announcement = event.announce
            print(announcement.captures)  # what * matched, or None for a partial overlap
            broadcast = await client.request_broadcast(announcement.prefix)
            catalog = await broadcast.catalog()
            name, track = next(iter(catalog.audio.items()))
            async for frame in await broadcast.subscribe_media(name, track):
                print(frame.timestamp_us, len(frame.payload))

asyncio.run(main())
```

```python
import asyncio, moq

async def main():
    # opus_init_bytes, payload, pts, and rgba come from your encoder or capture source.
    async with moq.Client("https://cdn.moq.dev/anon") as client:
        broadcast = client.create_broadcast("my-stream.hang")

        # Already-encoded frames: the catalog is filled from the bitstream
        audio = broadcast.publish_audio(moq.AudioFormat.OPUS, opus_init_bytes)
        audio.write_frame(payload, timestamp_us=0)
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
        events.write_frame(b'{"cmd": "ready"}', 0)
        status = broadcast.publish_json_snapshot("status", compression=True)
        status.update({"state": "live", "viewers": 42})

        broadcast.announce()
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
- **Live encoder timing.** After writing a frame you encoded yourself, call `flush(timestamp_us)` with the same timestamp so the catalog advertises your jitter. Skip it for file and network imports. On a seek or pause, call `discontinuity()`, then keep timestamps moving forward and resume video on a keyframe.
- **Decoded frames hold decoder buffers.** Drop each frame from `decode_video` promptly, or the decoder stalls. `resize` is best effort, so read each frame's `width()` and `height()`.
- **Closing.** `await session.shutdown()` gives finished tracks up to one second to deliver and raises if they did not. `cancel(code)` closes at once. Finish or abort live tracks first.
- **Stats.** `session.stats()` reports `rtt_us`, `estimated_send_rate_bps`, `estimated_recv_rate_bps`, and the byte and packet counters (`bytes_sent`, `bytes_received`, `bytes_lost`, `packets_sent`, `packets_received`, `packets_lost`). A field is `None` when the transport does not report it, which is not the same as zero.

## Reference

- API reference: [moq-rs.readthedocs.io](https://moq-rs.readthedocs.io)
- Source and examples: [`py/moq-rs`](https://github.com/moq-dev/moq/tree/main/py/moq-rs)
- Raw bindings: [`moq-ffi`](https://pypi.org/project/moq-ffi/) on PyPI, for the unwrapped API
