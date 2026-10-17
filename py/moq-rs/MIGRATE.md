# Migrating the Python moq-rs package

How to update your code for each breaking change, newest first.

## Unreleased

### Bandwidth reserves a track's demand (#5845)

`Bandwidth.reserve` takes the track's `demand()` instead of the producer.

```python
# Before
reservation = bandwidth.reserve(track, max_bps)
# After
reservation = bandwidth.reserve(track.demand(), max_bps)
```

### Media subscribe and fetch take a catalog rendition (#5834)

`media.TrackConsumer.subscribe` and `media.GroupConsumer.fetch` take the catalog ID and its `Video` or `Audio` config instead of a track name and container, and follow the rendition's broadcast and track; `fetch` takes them before `sequence`.
The decode consumers' `name` argument is `id`.

```python
# Before
track = audio.track if audio.track is not None else id
frames = await moq.media.TrackConsumer.subscribe(broadcast, track, audio.container)
# After
frames = await moq.media.TrackConsumer.subscribe(broadcast, id, audio)
```

### Native codecs have `audio` and `video` modules (#5694)

`BroadcastProducer.encode_audio` / `encode_video` are `moq.audio.Producer` / `moq.video.Producer`, built from a `moq.media.CatalogProducer`.
The audio track name moves into `EncoderOutput(track=...)`, and durations and timestamps are `timedelta` (`frame_duration`, `Frame.timestamp`) instead of `_us` integers; a zero `frame_duration` takes the codec's own frame.
`BroadcastConsumer.decode_audio` / `decode_video` are `await moq.audio.Consumer.subscribe(broadcast, ...)` / `await moq.video.Consumer.subscribe(broadcast, ...)`.
Producers expose subscriber state only through `demand()`, and a video `gop` is `moq.video.Gop.KEYFRAME(interval=...)`.
`moq.audio.Encoder` / `moq.video.Encoder` encode without publishing.

```python
# Before
audio = broadcast.encode_audio(
    "audio",
    moq.AudioEncoderInput(format=moq.AudioSampleFormat.F32, sample_rate=48000, channels=1),
    moq.AudioEncoderOutput(codec=moq.AudioCodec.opus(), frame_duration_us=20_000),
)
audio.write(moq.AudioFrame(timestamp_us=0, data=pcm))
# After
audio = moq.audio.Producer(
    moq.media.CatalogProducer(broadcast),
    moq.audio.EncoderInput(format=moq.audio.SampleFormat.F32, sample_rate=48000, channels=1),
    moq.audio.EncoderOutput(codec=moq.audio.Codec.opus(), track="audio", frame_duration=timedelta(milliseconds=20)),
)
audio.write(moq.audio.Frame(timestamp=timedelta(0), data=pcm))
```

### Background panics are reported (#5562)

A panic in a background task raises `moq.Error.Task` with the panic message as `reason`, and other objects keep running.

### Announcements are events with a restart

`AnnounceConsumer` yields an `AnnounceEvent`: `AnnounceEventStart`, `AnnounceEventUpdate`, `AnnounceEventEnd`, or `AnnounceEventRestart`, each carrying an `Announce` as `announce`.
`AnnounceUpdate` is gone.
A restart means another publisher instance now serves the prefix: request the path again.

```python
# Before
async for update in client.announced():
    if update.active:
        subscribe(update.prefix)
# After
async for event in client.announced():
    if isinstance(event, (moq.AnnounceEventStart, moq.AnnounceEventRestart)):
        subscribe(event.announce.prefix)
```

### "Epoch" means only the publisher epoch (#5146)

`Session.epoch()` is `connects()`, and `AudioProducer.reset_epoch()` is `reanchor()`.

```python
# Before
n = session.epoch()
audio.reset_epoch()
# After
n = session.connects()
audio.reanchor()
```

### The consume origin is `consume`

`Client(..., subscribe=origin)` is `Client(..., consume=origin)`.
A server request takes its origins in `accept(publish=..., consume=...)`, where `None` inherits the server's origin.
A value the native side cannot use raises `moq.Error.Config` at construction, including a bad bind address.

```python
# Before
client = moq.Client(url, publish=origin, subscribe=origin)
# After
client = moq.Client(url, publish=origin, consume=origin)
```

### JSON data tracks wrap a track

`BroadcastProducer.publish_json_snapshot` / `publish_json_stream` and the matching consumers are gone.
Create the track, then wrap it with `moq.json` (or `moq.flate` for opaque bytes).

```python
# Before
state = broadcast.publish_json_snapshot("state")
# After
track = broadcast.publish_track("state")
state = moq.json.SnapshotProducer(broadcast, track)
```

### Media lives in `moq.media`

`BroadcastProducer.publish_audio`, `publish_video`, `publish_container`, their `_on_track` and `_stream` variants, `set_video_properties`, and `set_catalog_section`, and the consumer's catalog and media subscriptions, are gone.
Use `moq.media.TrackProducer.audio` / `video`, `ContainerProducer`, `CatalogProducer`, `CatalogConsumer`, `TrackConsumer`, and `GroupConsumer`.
Producers drop `name`, `used`, and `unused`; read them through `demand()`.

```python
# Before
audio = broadcast.publish_audio(moq.AudioFormat.OPUS, opus_head)
# After
audio = moq.media.TrackProducer.audio(broadcast, moq.media.AudioInit(format=moq.media.AudioFormat.OPUS, data=opus_head))
```

### Container byte-stream importers take an init record

`moq.media.ContainerStreamProducer` takes a `ContainerStreamInit` holding the format instead of a bare format.
Its optional `timebase`, also on `ContainerInit`, shares one source's offset across container imports.

```python
# Before
producer = moq.media.ContainerStreamProducer(broadcast, moq.media.ContainerFormat.FMP4)
# After
producer = moq.media.ContainerStreamProducer(broadcast, moq.media.ContainerStreamInit(moq.media.ContainerFormat.FMP4))
```

### Durations are `timedelta`

`Frame`, `Datagram`, `Subscription`, `TrackInfo`, and `ConnectionStats` carry `timedelta` instead of microsecond integers: `timestamp_us` is `timestamp`, and `rtt_us` is `rtt`.
A string passed where a list of strings belongs (`tls_roots="ca.pem"`) raises `TypeError` instead of splitting into characters.

```python
# Before
track.write_frame(payload, timestamp_us=1_000)
# After
track.write_frame(payload, timestamp=timedelta(milliseconds=1))
```

### Timestamps are optional

A frame read from an untimed track has `timestamp` set to `None`.
Raw writes no longer default to zero, so a write to a timed track without a timestamp raises `TimestampMismatch`; whole-frame media writes require one.
`moq.media.CatalogProducer.now()` samples the broadcast clock for local-event stamps.

```python
# Before
track.write_frame(payload)
# After
track.write_frame(payload, timestamp=pts)
```

### Subscriber staleness is `max_delay`

`Subscription.max_age_us` is `max_delay`, a `timedelta`, and the decoder outputs take `max_delay`, also a `timedelta`.
`TrackInfo` keeps its `max_age`: it is the publisher's retention.

```python
# Before
sub = moq.Subscription(max_age_us=500_000)
# After
sub = moq.Subscription(max_delay=timedelta(milliseconds=500))
```

### Error payloads are named `reason`

Errors that carried a positional string (`Transport`, `Internal`, `Connect`, ...) name it `reason`.

```python
# Before
reason = err[0]
# After
reason = err.reason
```

### `Transport` gains `WEB_TRANSPORT`

A WebTransport session reports `moq.Transport.WEB_TRANSPORT`; `QUIC` now means raw QUIC only, so a server that admits only `QUIC` refuses every browser.

```python
# Before
quic = request.transport == moq.Transport.QUIC
# After
quic = request.transport in (moq.Transport.QUIC, moq.Transport.WEB_TRANSPORT)
```

### `stalled` is `enabled` (#4915)

`moq.media.Video.stalled` is `enabled`, defaulting to `True` with the meaning inverted, and `moq.media.Audio` gains `enabled`.

```python
# Before
live = not video.stalled
# After
live = video.enabled
```

### Routes have one cost (#4661)

`Route.cold` is removed.

```python
# Before
route = moq.Route(cost=10, cold=20)
# After
route = moq.Route(cost=10)
```

### Decoded video frames are objects

A `VideoDecodedFrame`'s fields are methods: `timestamp_us()`, `width()`, `height()`, and `pixels(format)` replacing `data` and `format`.
`VideoDecoderOutput.format` is gone; set `surface=True` to keep the decoder's native surface.

```python
# Before
rgba = frame.data
# After
rgba = frame.pixels(moq.VideoPixelFormat.RGBA)
```

### `BroadcastProducer.finish()` is removed (#4152)

Close the broadcast instead.

```python
# Before
broadcast.finish()
# After
broadcast.close()
```

## 0.4 -> 0.5

### Publish split by kind

`publish_media*` is `publish_audio`, `publish_video`, or `publish_container`, each taking its own init.
The raw encoder paths that used to be `publish_audio` / `publish_video` are `encode_audio` / `encode_video`.

```python
# Before
media = broadcast.publish_media("opus", opus_head)
# After
media = broadcast.publish_audio(moq.AudioFormat.OPUS, opus_head)
```

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

`Error.Protocol` carries a `ProtocolError` (scope, wire code, kind) instead of a flattened message.
Setters raise (`Busy` during connect or listen) instead of dropping the value.
Track and group `finish()` keep the handle open, so a later `abort()` can still run.

```python
# Before
except moq.Error.Protocol as e:
    message = str(e)
# After
except moq.Error.Protocol as e:
    kind = e.details.kind
```
