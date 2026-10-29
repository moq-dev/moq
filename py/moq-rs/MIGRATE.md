# Migrating the Python moq-rs package

How to update your code for each breaking change, newest first.

## Unreleased

### Compression moves to the track

`moq.flate` is removed: publish opaque bytes on a `TrackProducer`.
The `moq.json` producers and consumers drop their `compression` keyword. Publish the track with `TrackInfo(compress=True)` instead; a consumer reads a compressed track with no option of its own.

### Paths with an empty segment are refused ([#6648](https://github.com/moq-dev/moq/pull/6648))

Publishing, serving, or requesting a broadcast at a path with a leading, trailing, or doubled `/` raises instead of collapsing it. Pass the canonical path, such as `room/cam`.

### Request handlers are `requests()` ([#6752](https://github.com/moq-dev/moq/pull/6752))

Rename `dynamic()` on `OriginProducer`, `BroadcastProducer`, `TrackProducer`, and `TrackRequest` to `requests()`.
Their handles `OriginDynamic`, `BroadcastDynamic`, and `TrackDynamic` are `OriginRequests`, `BroadcastRequests`, and `TrackRequests`; their methods are unchanged.

### `Allocator` ([#6751](https://github.com/moq-dev/moq/pull/6751))

`moq.Bandwidth` is `moq.Allocator`, `Session.bandwidth()` is `allocator()`, and `Reservation.grant()` is `rate()`. `Bandwidth.Consumer`, returned by `recv_bandwidth()`, is the top-level `moq.BandwidthConsumer`. The producers' `bandwidth=` argument keeps its name.

### Decoders read gaps and ends

`audio.Consumer` yields `audio.Read` (`audio.Frame | audio.Gap | audio.End`) and `video.Consumer` yields `video.Read` (`VideoDecodedFrame | video.Gap | video.End`). Check `isinstance(read, audio.Frame)` (or `VideoDecodedFrame`) where you used each item as a frame; a `Gap` marks lost media and an `End` where the publisher stopped its encoder, and what follows either does not continue what came before.

### Audio frame duration is optional

`audio.EncoderOutput.frame_duration` defaults to `None` for the codec's own frame. Pass `None` where you passed `timedelta(0)`.
`FetchGroupOptions.max_delay` is a `timedelta` instead of `max_delay_us` microseconds, and `Track.info()` no longer raises.

### Connection state

Client entry returns a connecting native session. Replace `Session.status()`, `connects()`, and both `failure()` polls with `state()` and `await state_changed(state.revision)`. The `ConnectionState` fields include `phase`, `connects`, `failure`, and `version`; `ConnectionPhase` replaces `ConnectionStatus`. Wait for `CONNECTED` when work requires an established transport, or await `closed()` for the terminal result.

### Exact encoder rates and publication options

Raw video input framerates are exact numerator/denominator records, not integers.
Use `video.FrameRate(numerator=30000, denominator=1001)` and move `track=` from encoder output to `audio.Producer` / `video.Producer`.
The codec-only output records no longer contain track naming. Rebuild against
the matching generated bindings and native library.

### Video rendition preference

`moq.media.Video` gains `preference`, defaulting to `0`. Prefer keyword construction and upgrade `moq-ffi` alongside the wrapper. Higher values are preferred; compatibility transcodes use `-1`.

### `announce` takes an `AnnounceRoute`

`BroadcastProducer.announce` takes `moq.AnnounceRoute(cost=..., epoch=...)` instead of a `Route`: the origin builds the hop chain, so `hops` is no longer an input.
`Route` is only what announcements report. To re-announce an observed route, copy its `cost` and `epoch`.

### A broadcast reference waits for its sibling's announcement (#6254)

Resolving a rendition's `broadcast` reference, through `BroadcastConsumer.resolve()` and the rendition-based subscribes and fetches, now waits for a sibling nothing announces yet instead of failing as unroutable, so a late sibling plays once it is announced.
Where you skipped such a rendition on that error, bound the call yourself: cancel the task or wrap it in `asyncio.wait_for`.

### Session shutdown drains

`Session.shutdown()` still returns at once, but it now requests a draining close instead of cancelling.
`await session.closed()` waits for the drain and raises delivery failures, and a clean `async with` exit now waits for it too.
Call `session.cancel(0)` to close at once, as `shutdown()` did.
If you adopted the release line's async `drain()`, call `session.shutdown()` and then `await session.closed()` instead.

### Media track names belong to the target

Audio and video init records no longer carry a track name. Pass that name on the media producer's named target instead; init records describe only the codec and media.

### Broadcast `finish` is removed

`BroadcastProducer.finish()`, a deprecated alias for `close()`, is removed.
Call `BroadcastProducer.close()` where you called `finish()`.
To end the content first, call `moq.media.CatalogProducer.finish()`; the broadcast stays up until `close()`.

### JSON snapshot `delta_ratio` is `deltas`

`SnapshotProducer(delta_ratio=...)` is replaced by `deltas: bool = True`.
Pass `deltas=False` where you passed `delta_ratio=0`, and drop the argument otherwise.
Publish the track with a `refresh` ratio so a joiner gets the value restated in a fresh group.

### Priorities are signed (#6142)

`TrackInfo.priority`, `Subscription.priority`, and `FetchGroupOptions.priority` take `-128..=127`, higher first, and 0 is the unset midpoint.
`TrackInfo.priority` defaulted to 127 and now defaults to 0.
To keep a value's moq-lite byte, subtract 128 from it: 255 becomes 127, 128 becomes 0, and 0 becomes -128.

### A client asks only about what it watches (#6165)

On moq-lite, a client session asks its relay for announcements only under paths something watches, through `announced()` or `announced_broadcast()`, instead of under its whole scope at connect.
`OriginConsumer.request_broadcast()` watches nothing, so on a client it fails with an unroutable error for a path nothing watches, even when the relay announces it.
Resolve the path with `announced_broadcast()`, or hold `announced()` over its prefix while requesting.
moq-transport and moq-lite-03/04 sessions are unchanged.

### `ContainerFormat.FMP4` unwraps to Legacy (#6250)

A container import with `ContainerFormat.FMP4` now publishes one Legacy frame per sample instead of one CMAF frame per fragment.
Pass the new `ContainerFormat.CMAF` to keep publishing CMAF.

### Catalog references carry their epoch

`Video` and `Audio` renditions gain `epoch`, the publisher instance their `broadcast` reference names.
`BroadcastConsumer.resolve(reference)` becomes `resolve(id, rendition)`, taking the catalog rendition the reference belongs to and reading its `broadcast` and `epoch` from it.
Resolving, and the rendition-based consumers, raise for a rendition whose broadcast is proven to serve another epoch.

### Timestamps stop at 2^61 - 1 microseconds (#6168)

Writing a frame, or any other call that takes a timestamp, fails with `TimeOverflow` past `2^61 - 1` microseconds instead of `2^62 - 1`, the most moq-lite-05/06 can carry.
A received timestamp past it fails the group as a malformed track.
Clamp timestamps you build from unbounded input to `2^61 - 1`.

### Finished tracks stay until the broadcast ends (#6157)

A finished track now stays listed, with its catalog rendition and demand, until its broadcast closes.
Publish a replacement under a new name, and expect a finished track's demand watch to end with the broadcast.

### A claim answers by publishing (#5678)

`OriginProducer.dynamic(prefix)` takes no route, and `OriginDynamic.update` is gone.
Replace `request.accept(broadcast)` with `prepared = request.prepare(moq.mint_epoch())`, fill `prepared.broadcast`, then `prepared.publish()`, which announces it at the exact path under that epoch and returns its producer.

### Anchors can name a time (#6074)

`Anchor` gains a `TIME` variant, the group containing a presentation time.
`TrackConsumer.update` now raises on a time anchor an untimed track can't serve.

### Rebuild the matching raw bindings

The documentation update changes UniFFI's audio-producer method metadata checksums.
Rebuild or reinstall the complete `moq-ffi` wheel, keeping its generated Python code
and native library together; application calls are unchanged.

### JSON consumers yield `json.Timed` (#5677)

`json.SnapshotConsumer` and `json.StreamConsumer` yield `Timed(value, timestamp)` instead of the bare value; `timestamp` is a `timedelta`, or `None` when untimed.
Iterating a snapshot consumer now yields every state in order; `await consumer.latest()` returns the newest.
Timestamps are rounded down to a whole microsecond.

### Ranges filter reads (#5929)

A subscription now reads only the groups its own `ranges` cover.

### Track demand reports the wanted group (#5930)

`TrackDemand.is_used()`, `used()`, and `unused()` are gone.
`wanted()` returns `None` while nobody subscribes, otherwise the group subscribers want next, and `await wanted_changed()` waits for it to change (the first call returns at once).
Loop on `wanted_changed()` until it returns non-`None` where you awaited `used()`, and `None` where you awaited `unused()`.
`TrackInfo` gains `refresh`.

### `CongestionControl` gained `NONE` (#5926)

`CongestionControl.NONE` runs no congestion control and no pacing, for tests and controlled links.
Code that handles every value needs one more case.

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Pass `keylog=True` to `Client`, `connect`, or `Server` as well to capture them; without `SSLKEYLOGFILE` the client refuses to build and the server refuses to listen.

### Subscriptions carry group ranges (#5732)

`Subscription.group_start` and `group_end` are replaced by `ranges`, a list of `Range` whose ends are an `Anchor` (an absolute group and frame, or a count of groups before the latest), and an optional `order`.
To start at group `n`, pass one range starting at the absolute anchor for group `n`, frame 0.

### Receive estimates come from `Session.recv_bandwidth` (#5777)

The connection stats no longer carry a receive estimate.
Hold the consumer `Session.recv_bandwidth` returns to request PROBE, read it with `rate()` or wait with `rate_changed()`, and drop it to stop.

```python
bandwidth = session.recv_bandwidth()
rate = bandwidth.rate() if bandwidth else None
```

### `AnnouncedBroadcast.available()` waits for the next publisher instance (#5700)

The first call returns the broadcast serving the path, as before.
Each later call waits for the next instance to take over the path, instead of returning whatever serves it now.

### A timed track refuses a group that rewinds (#5803)

Writing a frame fails with `TimestampMismatch` when a group's first frame starts before a lower-sequence group's, or after a higher one's.
A publisher whose timeline restarts publishes a new broadcast instead.

### Bandwidth reserves a track's demand (#5845)

`Bandwidth.reserve` takes the track's `demand()` instead of the producer.

### Media subscribe and fetch take a catalog rendition (#5834)

`media.TrackConsumer.subscribe` and `media.GroupConsumer.fetch` take the catalog ID and its `Video` or `Audio` config instead of a track name and container, and follow the rendition's broadcast and track; `fetch` takes them before `sequence`.
The decode consumers' `name` argument is `id`.

### Native codecs have `audio` and `video` modules (#5694)

`BroadcastProducer.encode_audio` / `encode_video` are `moq.audio.Producer` / `moq.video.Producer`, built from a `moq.media.CatalogProducer`.
The audio track name belongs to `Producer(track=...)`, and durations and timestamps are `timedelta` (`frame_duration`, `Frame.timestamp`) instead of `_us` integers; a zero `frame_duration` takes the codec's own frame.
`BroadcastConsumer.decode_audio` / `decode_video` are `await moq.audio.Consumer.subscribe(broadcast, ...)` / `await moq.video.Consumer.subscribe(broadcast, ...)`.
Producers expose subscriber state only through `demand()`, and a video `gop` is `moq.video.Gop.KEYFRAME(interval=...)`.
`moq.audio.Encoder` / `moq.video.Encoder` encode without publishing.

```python
audio = moq.audio.Producer(
    moq.media.CatalogProducer(broadcast),
    moq.audio.EncoderInput(format=moq.audio.SampleFormat.F32, sample_rate=48000, channels=1),
    moq.audio.EncoderOutput(codec=moq.audio.Codec.opus(), frame_duration=timedelta(milliseconds=20)),
    track="audio",
)
audio.write(moq.audio.Frame(timestamp=timedelta(0), data=pcm))
```

### Background panics are reported (#5562)

A panic in a background task raises `moq.Error.Task` with the panic message as `reason`, and other objects keep running.

### Announcements are events with a restart

`AnnounceConsumer` yields an `AnnounceEvent`: `AnnounceEventStart`, `AnnounceEventUpdate`, `AnnounceEventEnd`, or `AnnounceEventRestart`, each carrying an `Announce` as `announce`.
`AnnounceUpdate` is gone.
A restart means another publisher instance now serves the prefix: request the path again.
Where you checked `update.active`, act on `AnnounceEventStart` and `AnnounceEventRestart` and read `event.announce.prefix`.

### "Epoch" means only the publisher epoch (#5146)

`Session.epoch()` is `connects()`, and `AudioProducer.reset_epoch()` is `reanchor()`.

### The consume origin is `consume`

`Client(..., subscribe=origin)` is `Client(..., consume=origin)`.
A server request takes its origins in `accept(publish=..., consume=...)`, where `None` inherits the server's origin.
A value the native side cannot use raises `moq.Error.Config` at construction, including a bad bind address.

### JSON data tracks wrap a track

`BroadcastProducer.publish_json_snapshot` / `publish_json_stream` and the matching consumers are gone.
Create the track, then wrap it with `moq.json` (or `moq.flate` for opaque bytes).

```python
track = broadcast.publish_track("state")
state = moq.json.SnapshotProducer(broadcast, track)
```

### Media lives in `moq.media`

`BroadcastProducer.publish_audio`, `publish_video`, `publish_container`, their `_on_track` and `_stream` variants, `set_video_properties`, and `set_catalog_section`, and the consumer's catalog and media subscriptions, are gone.
Use `moq.media.TrackProducer.audio` / `video`, `ContainerProducer`, `CatalogProducer`, `CatalogConsumer`, `TrackConsumer`, and `GroupConsumer`.
Producers drop `name`, `used`, and `unused`; read them through `demand()`.

```python
audio = moq.media.TrackProducer.audio(broadcast, moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=opus_head))
```

### Container byte streams use `moq.media.ContainerProducer` (#5919)

`BroadcastProducer.publish_container_stream` has no separate importer: a container recovers its own framing, so `moq.media.ContainerProducer` takes chunks split anywhere.
Pass empty `data` and write the stream through it.
`ContainerInit`'s optional `timebase` shares one source's offset across container imports.

```python
stream = moq.media.ContainerProducer(broadcast, moq.media.ContainerInit(moq.media.ContainerFormat.FMP4, b""))
```

### Durations are `timedelta`

`Frame`, `Datagram`, `Subscription`, `TrackInfo`, and `ConnectionStats` carry `timedelta` instead of microsecond integers: `timestamp_us` is `timestamp`, and `rtt_us` is `rtt`.
A string passed where a list of strings belongs (`tls_roots="ca.pem"`) raises `TypeError` instead of splitting into characters.

### Timestamps are optional

An omitted timescale now publishes a raw track untimed instead of selecting microseconds. Explicitly set a microsecond timescale to preserve the released default.

A frame read from an untimed track has `timestamp` set to `None`.
Raw writes no longer default to zero, so a write to a timed track without a timestamp raises `TimestampMismatch`; whole-frame media writes require one.
Pass `timestamp=` on every write to a timed track.
`moq.media.CatalogProducer.now()` samples the broadcast clock for local-event stamps.

### Subscriber staleness is `max_delay`

`Subscription.max_age_us` is `max_delay`, a `timedelta`, and the decoder outputs take `max_delay`, also a `timedelta`.
`TrackInfo` keeps its `max_age`: it is the publisher's retention.

### Error payloads are named `reason`

Errors that carried a positional string (`Transport`, `Internal`, `Connect`, ...) name it `reason`: read `err.reason` instead of `err[0]`.

### `Transport` gains `WEB_TRANSPORT`

A WebTransport session reports `moq.Transport.WEB_TRANSPORT`; `QUIC` now means raw QUIC only, so a server that admits only `QUIC` refuses every browser.
Check for both where you meant any QUIC session.

### `stalled` is `enabled` (#4915)

`moq.media.Video.stalled` is `enabled`, defaulting to `True` with the meaning inverted, and `moq.media.Audio` gains `enabled`.

### Routes have one cost (#4661)

`Route.cold` is removed: drop the argument and keep `cost`.

### Decoded video frames are objects

A `VideoDecodedFrame`'s fields are methods: `timestamp_us()`, `width()`, `height()`, and `pixels(format)` replacing `data` and `format`.
`VideoDecoderOutput.format` is gone; set `surface=True` to keep the decoder's native surface.
Read `frame.pixels(moq.VideoPixelFormat.RGBA)` instead of `frame.data`.

## 0.4 -> 0.5

### Publish split by kind

`publish_media*` is `publish_audio`, `publish_video`, or `publish_container`, each taking its own init.
For example, `publish_media("opus", opus_head)` is `publish_audio(moq.AudioFormat.OPUS, opus_head)`.
The raw encoder paths that used to be `publish_audio` / `publish_video` are `encode_audio` / `encode_video`.

### Renames

| Before | After |
| --- | --- |
| millisecond durations | microseconds (`max_age_us`, `Backoff.initial_us`) |
| `estimated_send_bitrate` / `estimated_recv_bitrate` | `estimated_send_rate` / `estimated_recv_rate` |
| `tls_disable_verify=True` | `tls_verify=False` |
| `Announced` / `Announcement` | `AnnounceConsumer` / `AnnounceUpdate` |
| `BroadcastRequest.abort` | `reject` |
| `OriginOptions` | `OriginConfig` |

### Errors are structured

`Error.Protocol` carries a `ProtocolError` (scope, wire code, kind) instead of a flattened message: read `e.details.kind` instead of parsing `str(e)`.
Setters raise (`Busy` during connect or listen) instead of dropping the value.
Track and group `finish()` keep the handle open, so a later `abort()` can still run.
