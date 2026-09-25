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
        # The filter is relative to the literal prefix; updates stay origin-relative.
        async for announcement in client.announced("live/", filter="*/camera"):
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

asyncio.run(main())
```

For already-encoded live output, call `audio.flush(timestamp_us)` after each `audio.write_frame` with the same broadcast-clock PTS. It samples the transport handoff for catalog jitter. File, pipe, and network imports should omit `flush`; raw-pixel and PCM encoders inside the binding measure their own output.

The three advertising operations, as the other bindings spell them:
`client.create_broadcast(path)` (or `OriginProducer.create_broadcast`) returns
an unannounced producer, invisible to everyone; `broadcast.announce(route)` /
`broadcast.unannounce()` own that exact-path advertisement, and
`broadcast.close()` ends the broadcast for good (a second call is a no-op;
`finish()` is its deprecated alias);
`origin.dynamic(prefix, route)` claims `prefix` and every path beneath it
(`""` for everything). Hold the returned handle while the claim should stay
advertised, and reject the requests you will not serve. A route is a
capability, not an inventory. `announced(prefix, filter=...)` combines a literal
root with an optional relative pattern; each announcement `.prefix` stays
relative to the origin and `.captures` reports what the pattern wildcards matched.
Paths with a `.`-prefixed segment below the prefix are [hidden](/concept/moq-lite#hidden-broadcasts) unless
`hidden=True`.

Sessions reconnect with backoff when the transport drops and re-announce local
broadcasts. `session.epoch()` counts the connections, 1 on the first, pairing
with `session.status()` to log each reconnect; `moq.Backoff` tunes the pacing
(`timeout_us=0` retries forever); and `moq.connect(..., max_streams=...)`
raises the peer's inbound stream cap.

The [WebSocket fallback](/concept/transport#websocket-fallback) races QUIC after
a 200 ms head start. Pass `websocket_enabled=False` to `moq.connect` for a
QUIC-only relay, or a `websocket_delay` `timedelta` to change the head start.

Everything in the [shared feature list](/lib/#what-every-binding-can-do) is
here: `moq.Server` with per-request accept/reject, `fetch_group` and
`fetch_media_group`, `dynamic()` handlers for on-demand tracks and
`dynamic(prefix)` for broadcasts, `append_datagram`/`recv_datagram`, `set_catalog_section`,
and a producer's `demand()`, a `TrackDemand` whose
`used()`/`unused()` let capture idle when nobody is subscribed. `request.set_publish`/`set_consume` raise if the request is already
answered, cancelled, or currently accepting. `session.bandwidth()` divides the connection's send estimate;
pass it to `encode_video` / `encode_audio` or `reserve` a share for an
app-owned track. `moq.is_auth(err)` and `moq.is_shutdown(err)` classify errors. `moq.protocol_error(err)` is the structured protocol failure (scope, verbatim code, kind) when the peer sent one. Catch `moq.Error.Busy` when a setter races an in-flight connect, listen, or accept.
Each server request reports a `moq.Transport` enum, including QUIC, Iroh,
WebSocket, TCP, and Unix sockets.

`encode_audio` encodes raw PCM inside the binding. Its codec is an object,
`moq.AudioCodec.opus()`, and `AudioEncoderOutput.frame_duration_us` sets the
Opus frame length: 2500, 5000, 10000, 20000 (the default), 40000, or 60000.

`decode_video` picks the decoded CPU pixel layout: `VideoDecoderOutput.format`
is `VideoPixelFormat.I420` when unset, or `VideoPixelFormat.RGBA` for four
bytes a pixel, and every frame repeats the layout it was decoded to. `resize`
is best effort: only NVDEC has a built-in scaler, so read each frame's own
`width` and `height` rather than assuming it took.

## Connection stats

`session.stats()` returns a `ConnectionStats` snapshot. Each field is `None`
when the transport backend does not report it (native QUIC reports all of them;
browser WebTransport reports few or none) or before it is available, which is
not the same as zero.

| Field | Unit | Meaning |
| --- | --- | --- |
| `rtt_us` | microseconds | Smoothed round-trip time. |
| `estimated_send_rate_bps` | bits per second | Send bandwidth from the congestion controller. |
| `estimated_recv_rate_bps` | bits per second | Receive bandwidth from MoQ PROBE. |
| `bytes_sent` | bytes | Total sent, including retransmissions and overhead. |
| `bytes_received` | bytes | Total received, including duplicates and overhead. |
| `bytes_lost` | bytes | Total lost, detected via retransmission or acknowledgement. |
| `packets_sent` | datagrams | Total datagrams sent. |
| `packets_received` | datagrams | Total datagrams received. |
| `packets_lost` | datagrams | Total datagrams detected as lost. |

- API reference: [moq-rs.readthedocs.io](https://moq-rs.readthedocs.io)
- Source and examples: [`py/moq-rs`](https://github.com/moq-dev/moq/tree/main/py/moq-rs)
- Raw bindings: [`moq-ffi`](https://pypi.org/project/moq-ffi/) on PyPI, for the unwrapped API
