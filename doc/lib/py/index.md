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
        # Subscribe to media
        async for announcement in client.announced("live/"):
            catalog = await announcement.broadcast.catalog()
            name, track = next(iter(catalog.audio.items()))
            async for frame in await announcement.broadcast.subscribe_media(name, track):
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
        audio = broadcast.publish_media("opus", opus_init_bytes)
        audio.write_frame(payload, timestamp_us=0)

        # Or raw pixels, encoded inside the binding (VideoToolbox, Media Foundation, NVENC, openh264)
        video = broadcast.publish_video(
            moq.VideoEncoderInput(format=moq.VideoPixelFormat.RGBA, width=1280, height=720, framerate=30),
            moq.VideoEncoderOutput(codec=moq.VideoCodec.H264, track="camera", kind=moq.VideoEncoderKind.AUTO()),
        )
        video.write(moq.VideoFrame(timestamp_us=pts, data=rgba))

        # Raw bytes and JSON
        events = broadcast.publish_track("events")
        events.write_frame(b'{"cmd": "ready"}', 0)
        status = broadcast.publish_json_snapshot("status", compression=True)
        status.update({"state": "live", "viewers": 42})

asyncio.run(main())
```

Everything in the [shared feature list](/lib/#what-every-binding-can-do) is
here: `moq.Server` with per-request accept/reject, `fetch_group` and
`fetch_media_group`, `dynamic()` handlers for on-demand tracks and
broadcasts, `append_datagram`/`recv_datagram`, `set_catalog_section`,
`route_updates()`, and `used()`/`unused()` so capture can idle when nobody is
subscribed. `moq.is_auth(err)` and `moq.is_shutdown(err)` classify errors.

- API reference: [moq-rs.readthedocs.io](https://moq-rs.readthedocs.io)
- Source and examples: [`py/moq-rs`](https://github.com/moq-dev/moq/tree/main/py/moq-rs)
- Raw bindings: [`moq-ffi`](https://pypi.org/project/moq-ffi/) on PyPI, for the unwrapped API
