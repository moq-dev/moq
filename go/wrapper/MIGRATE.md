# Migrating the Go moq.dev/moq module

How to update your code for each breaking change, newest first.

## Unreleased

### Bandwidth reserves a track's demand (#5845)

`Bandwidth.Reserve` takes the track's `Demand()` instead of the producer, and no longer returns an error.

```go
// Before
reservation, err := bandwidth.Reserve(track, maxBps)
// After
demand, err := track.Demand()
reservation := bandwidth.Reserve(demand, maxBps)
```

### Media subscribe and fetch take a catalog rendition (#5834)

`media.TrackConfig` and `media.GroupConfig` replace `Name` and `Container` with a `Rendition`, built by `media.VideoRendition` or `media.AudioRendition`, and follow its broadcast and track themselves.

```go
// Before
config := media.TrackConfig{Name: id, Container: video.Container}
// After
config := media.TrackConfig{Rendition: media.VideoRendition(id, video)}
```

### Native codecs have `audio` and `video` packages (#5694)

`BroadcastProducer.EncodeAudio` / `EncodeVideo` are `audio.NewProducer` / `video.NewProducer` (`moq.dev/moq/audio`, `moq.dev/moq/video`), built from a `media.CatalogProducer`.
The audio track name moves into `EncoderOutput.Track`, and durations and timestamps are `time.Duration` (`FrameDuration`, `audio.Frame.Timestamp`, `video.Frame.Timestamp`); a zero `FrameDuration` takes the codec's own frame.
`BroadcastConsumer.DecodeAudio` / `DecodeVideo` are `audio.Subscribe` / `video.Subscribe`, taking the broadcast after the context.
Producers expose subscriber state only through `Demand()`, and a video GOP is `video.Keyframe(interval)`.
`audio.NewEncoder` / `video.NewEncoder` encode without publishing.

```go
// Before
video, err := broadcast.EncodeVideo(moq.VideoEncoderInput{...}, moq.VideoEncoderOutput{...}, nil)
err = video.Write(moq.VideoFrame{TimestampUs: pts, Data: rgba})
frames, err := consumer.DecodeVideo(ctx, name, rendition, output)
// After
catalog, err := moqmedia.NewCatalogProducer(broadcast)
video, err := moqvideo.NewProducer(catalog, moqvideo.EncoderInput{...}, moqvideo.EncoderOutput{...}, nil)
err = video.Write(moqvideo.Frame{Timestamp: time.Duration(pts) * time.Microsecond, Data: rgba})
frames, err := moqvideo.Subscribe(ctx, consumer, name, rendition, output)
```

### Background panics are reported (#5562)

A panic in a background task returns an `ErrTask` error carrying the panic message, and other objects keep running.

### Announcements are events with a restart

`AnnounceConsumer.Next` and `All` yield an `AnnounceEvent`: `AnnounceEventStart`, `AnnounceEventUpdate`, `AnnounceEventEnd`, or `AnnounceEventRestart`, each carrying an `Announce`.
`AnnounceUpdate` and its `Active()` are gone.
A restart means another publisher instance now serves the prefix: request the path again.

```go
// Before
for update, err := range announced.All(ctx) {
	if err == nil && update.Active() {
		subscribe(update.Prefix())
	}
}
// After
for event, err := range announced.All(ctx) {
	switch e := event.(type) {
	case moq.AnnounceEventStart:
		subscribe(e.Announce.Prefix)
	case moq.AnnounceEventRestart:
		subscribe(e.Announce.Prefix)
	}
}
```

### "Epoch" means only the publisher epoch (#5146)

`Session.Epoch()` is `Connects()`, and `AudioProducer.ResetEpoch()` is `Reanchor()`.

```go
// Before
n := session.Epoch()
audio.ResetEpoch()
// After
n := session.Connects()
audio.Reanchor()
```

### Origins are `consume`, and servers accept with them

`WithSubscribeOrigin` is `WithConsumeOrigin`.
A server request's `SetPublish` / `SetConsume` are arguments to `Accept(ctx, publish, consume)`, where `nil` inherits the server's origin.
`Server.Requests(ctx)` is `All(ctx)`, and the `Status*` constants are `ConnectionStatus*`.
A value the native side cannot use fails `Dial` with a config error, including a bad bind address.

```go
// Before
client, err := moq.Dial(ctx, url, moq.WithSubscribeOrigin(origin))
for request, err := range server.Requests(ctx) {
	session, err := request.Accept(ctx)
}
// After
client, err := moq.Dial(ctx, url, moq.WithConsumeOrigin(origin))
for request, err := range server.All(ctx) {
	session, err := request.Accept(ctx, nil, nil)
}
```

### Media lives in `moq.dev/moq/media`

`BroadcastProducer.PublishAudio`, `PublishVideo`, `PublishContainer`, their `OnTrack` and `Stream` variants, `SetVideoProperties`, and `SetCatalogSection`, and the consumer's catalog and media subscriptions, are gone.
Use `media.NewAudioTrackProducer` / `NewVideoTrackProducer`, `NewContainerProducer`, `NewCatalogProducer`, `NewCatalogConsumer`, `NewTrackConsumer`, and `media.FetchGroup`; `media.NewGroupConsumer` decodes a raw group.
Producers drop `Name`, `Used`, and `Unused`; read them through `Demand()`.
JSON and opaque data tracks live in `moq.dev/moq/json` and `moq.dev/moq/flate`, wrapping a track from `PublishTrack`.

```go
// Before
audio, err := broadcast.PublishAudio(moq.AudioFormatOpus, opusHead)
// After
audio, err := media.NewAudioTrackProducer(broadcast, media.Named{}, media.AudioInit{Format: media.AudioFormatOpus, Data: opusHead})
```

### Container byte-stream importers take an init record

`media.NewContainerStreamProducer` takes a `ContainerStreamInit` holding the format instead of a bare format.
Its optional `Timebase`, also on `ContainerInit`, shares one source's offset across container imports.

```go
// Before
producer, err := media.NewContainerStreamProducer(broadcast, media.ContainerFormatFmp4)
// After
producer, err := media.NewContainerStreamProducer(broadcast, media.ContainerStreamInit{Format: media.ContainerFormatFmp4})
```

### Durations are `time.Duration`

`Frame`, `Datagram`, `Subscription`, `TrackInfo`, and `ConnectionStats` carry `time.Duration` instead of microsecond integers: `TimestampUs` is `Timestamp`, and `RttUs` is `RTT`.
A frame timestamp is a `*time.Duration`, `nil` on a frame read from an untimed track.

```go
// Before
err := track.WriteFrame(moq.Frame{Payload: payload, TimestampUs: 1000})
// After
ts := time.Millisecond
err := track.WriteFrame(moq.Frame{Payload: payload, Timestamp: &ts})
```

### Timestamps are optional

A raw write to a timed track without a timestamp fails with `TimestampMismatch` instead of going out at 0, and whole-frame media writes require one.
`media.CatalogProducer.Now()` samples the broadcast clock for local-event stamps.

```go
// Before
err := track.WriteFrame(moq.Frame{Payload: payload})
// After
err := track.WriteFrame(moq.Frame{Payload: payload, Timestamp: &pts})
```

### Subscriber staleness is `MaxDelay`

`Subscription.MaxAgeUs` is `MaxDelay`, and `audio.DecoderOutput` / `video.DecoderOutput` take `MaxDelay *time.Duration`.
`TrackInfo.MaxAge` keeps its name: it is the publisher's retention.

```go
// Before
sub := moq.Subscription{MaxAgeUs: 500_000}
// After
sub := moq.Subscription{MaxDelay: 500 * time.Millisecond}
```

### Error payloads are named `Reason`

Errors that carried a positional string (`Transport`, `Internal`, `Connect`, ...) name it `Reason`.

```go
// Before
reason := transport.Field0 // *ffi.MoqErrorTransport from errors.As
// After
reason := transport.Reason
```

### `Transport` gains `WebTransport`

A WebTransport session reports `TransportWebTransport`; `TransportQUIC` now means raw QUIC only.

```go
// Before
quic := request.Transport() == moq.TransportQUIC
// After
quic := request.Transport() == moq.TransportQUIC || request.Transport() == moq.TransportWebTransport
```

### `Stalled` is `Enabled` (#4915)

`media.Video.Stalled` is `Enabled`, defaulting to true with the meaning inverted, and `media.Audio` gains `Enabled`.

```go
// Before
live := !video.Stalled
// After
live := video.Enabled
```

### Routes have one cost (#4661)

`Route.Cold` is removed.

```go
// Before
route := moq.Route{Cost: 10, Cold: &cold}
// After
route := moq.Route{Cost: 10}
```

### Decoded video frames are objects

`VideoDecodedFrame` is a handle with `TimestampUs()`, `Width()`, `Height()`, `Pixels(format)`, `Surface()`, and `Close()`.
`VideoDecoderOutput.Format` is gone; set `Surface: true` to keep the decoder's native surface.

```go
// Before
rgba := frame.Data
// After
rgba, err := frame.Pixels(moq.VideoPixelFormatRgba)
frame.Close()
```

### `BroadcastProducer.Finish()` is removed (#4152)

```go
// Before
broadcast.Finish()
// After
broadcast.Close()
```

## 0.5 -> 0.7

### The module path is `moq.dev/moq`

```go
// Before
import "github.com/moq-dev/moq-go/moq"
// After
import "moq.dev/moq"
```

### Publish split by kind

`PublishMedia*` is `PublishAudio`, `PublishVideo`, or `PublishContainer`, each taking its own init.
The raw encoder paths that used to be `PublishAudio` / `PublishVideo` are `EncodeAudio` / `EncodeVideo`.

```go
// Before
media, err := broadcast.PublishMedia("opus", opusHead)
// After
media, err := broadcast.PublishAudio(moq.AudioFormatOpus, opusHead)
```

### Renames

| Before | After |
| --- | --- |
| millisecond durations | microseconds (`MaxAgeUs`, `Backoff.InitialUs`) |
| `EstimatedSendBitrate` / `EstimatedRecvBitrate` | `EstimatedSendRate` / `EstimatedRecvRate` |
| `WithTLSDisableVerify(true)` | `WithTLSVerify(false)` |
| `Announced` / `Announcement` | `AnnounceConsumer` / `AnnounceUpdate` |
| `BroadcastRequest.Abort` | `Reject` |
| `OriginOptions` | `OriginConfig` |

### Errors are structured

Protocol errors carry a `ProtocolError` (scope, wire code, kind) instead of a flattened message.
Setters return an error (`Busy` during connect or listen) instead of dropping the value.
Track and group `Finish()` keep the handle open, so a later `Abort()` can still run.

```go
// Before
if errors.Is(err, moq.ErrProtocol) { log.Print(err.Error()) }
// After
if p, ok := moq.ProtocolError(err); ok { log.Print(p.Kind, p.Code) }
```
