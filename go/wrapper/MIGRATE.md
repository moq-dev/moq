# Migrating the Go moq.dev/moq module

How to update your code for each breaking change, newest first.

## Unreleased

### Compression moves to the track

`moq.dev/moq/flate` is removed: publish opaque bytes on a `TrackProducer`.
`SnapshotOptions` and `WindowOptions` lose `Compression`, and `StreamOptions` and `ConsumerOptions` are removed: `NewStreamProducer` and every JSON consumer constructor take only the track (and broadcast).
Publish the track with `&moq.TrackInfo{Compress: true}` instead; a consumer reads a compressed track with no option of its own.

### Paths with an empty segment are refused ([#6648](https://github.com/moq-dev/moq/pull/6648))

Publishing, serving, or requesting a broadcast at a path with a leading, trailing, or doubled `/` returns an error instead of collapsing it. Pass the canonical path, such as `room/cam`.

### Request handlers are `Requests()` ([#6752](https://github.com/moq-dev/moq/pull/6752))

Rename `Dynamic()` on `OriginProducer`, `BroadcastProducer`, `TrackProducer`, and `TrackRequest` to `Requests()`.
Their handles `OriginDynamic`, `BroadcastDynamic`, and `TrackDynamic` are `OriginRequests`, `BroadcastRequests`, and `TrackRequests`; their methods are unchanged.

### `Allocator` ([#6751](https://github.com/moq-dev/moq/pull/6751))

`Bandwidth` is `Allocator`, `Session.Bandwidth()` is `Session.Allocator()`, and `Reservation.Grant()` is `Reservation.Rate()`. The `Bandwidth` field of `ProducerOptions` keeps its name and takes a `*moq.Allocator`.

### Decoders read gaps and ends

`audio.Consumer.Next` and `video.Consumer.Next` return a `Read` (and `All` ranges over them) instead of a frame pointer. Switch on `ReadFrame` and take its `Frame` where you used the frame; `ReadGap` marks lost media and `ReadEnd` where the publisher stopped its encoder, and what follows either does not continue what came before. A nil `Read` still means the track ended.

### Durations cross as `time.Duration`

The re-exported records carry `time.Duration` instead of microseconds: `FetchGroupOptions.MaxDelay`, `media.Audio.Jitter`, and the raw `AudioDecoderOutput.{MaxDelay, Delay}`, `AudioEncoderOutput.FrameDuration`, and `VideoDecoderOutput.MaxDelay`, each a `*time.Duration` where it was a `*uint64` `...Us` field.
`TrackConsumer.Info()` returns only the `TrackInfo`, since it can no longer fail.
A sub-microsecond `Backoff` duration now reaches the native side as given instead of flooring at 1us.

### Connection state

`Dial` returns a connecting native client. Replace `Session.Status`, `Connects`, and `Failure` with `State()` and `StateChanged(ctx, state.Revision)`. `ConnectionState` carries `Phase`, `Connects`, `Failure`, and `Version`; `ConnectionPhase` replaces `ConnectionStatus`. Cancelling a state wait only cancels that wait. Cancel the session itself to stop its dial.

### Exact encoder rates and publication options

Raw video input framerates are exact numerator/denominator records, not integers.
Use `video.FrameRate{Numerator: 30000, Denominator: 1001}` and construct producers with `(catalog, ProducerOptions{Input: input, Output: output, Track: track, Bandwidth: bandwidth})` under the matching audio/video package.
The codec-only output records no longer contain track naming. Rebuild against
the matching generated bindings and native library.

### Video rendition preference

`media.Video` gains `Preference int32`. Update unkeyed struct literals or use keyed fields, leaving `Preference: 0` for the default ranking. Upgrade the matching FFI module too. Higher values are preferred; compatibility transcodes use `-1`.

### `Announce` takes an `AnnounceRoute`

`BroadcastProducer.Announce` takes `moq.AnnounceRoute{Cost, Epoch}` instead of a `moq.Route`: the origin builds the hop chain, so `Hops` is no longer an input, and `ErrInvalidRoute` is gone.
`Route` is only what announcements report. To re-announce an observed route, copy its `Cost` and `Epoch`.

### A broadcast reference waits for its sibling's announcement (#6254)

Resolving a rendition's `broadcast` reference, through `BroadcastConsumer.Resolve` and the rendition-based subscribes and fetches, now waits for a sibling nothing announces yet instead of failing as unroutable, so a late sibling plays once it is announced.
Where you skipped such a rendition on that error, bound the call yourself: pass a `ctx` with a deadline.

### Shutdown drains

`Session.Shutdown()` keeps its signature, but it now requests a draining close instead of cancelling.
Call `Session.Closed(ctx)` to wait for the drain and receive delivery failures, or `Session.Cancel(0)` to close at once, as `Shutdown()` did.
`Client.Close()` now waits for the drain and returns its error instead of always `nil`; allow for that wait in cleanup.

### Media track names belong to the target

Audio and video init records no longer carry a track name. Pass that name on the media producer's named target instead; init records describe only the codec and media.

### Broadcast `Finish` is removed

`BroadcastProducer.Finish()`, a deprecated alias for `Close()`, is removed.
Call `BroadcastProducer.Close()` where you called `Finish()`.
To end the content first, call `media.CatalogProducer.Finish()`; the broadcast stays up until `Close()`.

### JSON snapshot `DeltaRatio` is `Deltas`

`json.SnapshotOptions.DeltaRatio *uint32` and `json.DefaultDeltaRatio` are replaced by `Deltas *bool` (nil enables deltas).
Set `Deltas` to a pointer to `false` where you set `DeltaRatio` to a pointer to `0`, and leave it nil otherwise.
Publish the track with a `Refresh` ratio so a joiner gets the value restated in a fresh group.

### Priorities are signed (#6142)

`TrackInfo.Priority`, `Subscription.Priority`, and `FetchGroupOptions.Priority` are `int8`, and `GroupRequest.Priority()` returns one.
Higher is first and the zero value is the unset midpoint, so a `TrackInfo` that leaves `Priority` zero now gets the default rather than the least urgent priority; drop any `Priority: 127` that asked for the midpoint.
To keep a value's moq-lite byte, subtract 128 from it: 255 becomes 127, 128 becomes 0, and 0 becomes -128.

### A client asks only about what it watches (#6165)

On moq-lite, a client session asks its relay for announcements only under paths something watches, through `Announced()` or `AnnouncedBroadcast()`, instead of under its whole scope at connect.
`OriginConsumer.RequestBroadcast()` watches nothing, so on a client it fails with an unroutable error for a path nothing watches, even when the relay announces it.
Resolve the path with `AnnouncedBroadcast()`, or hold `Announced()` over its prefix while requesting.
moq-transport and moq-lite-03/04 sessions are unchanged.

### `media.ContainerFormatFmp4` unwraps to Legacy (#6250)

A container import with `media.ContainerFormatFmp4` now publishes one Legacy frame per sample instead of one CMAF frame per fragment.
Pass the new `media.ContainerFormatCmaf` to keep publishing CMAF.

### Catalog references carry their epoch

`ffi.MoqVideo` and `ffi.MoqAudio` gain `Epoch`, the publisher instance their `Broadcast` reference names.
`BroadcastConsumer.Resolve(ctx, reference)` becomes `Resolve(ctx, rendition)`, taking the catalog rendition (`media.VideoRendition` or `media.AudioRendition`) and reading its `Broadcast` and `Epoch` from it.
Resolving, and the rendition-based consumers, error for a rendition whose broadcast is proven to serve another epoch.

### Timestamps stop at 2^61 - 1 microseconds (#6168)

Writing a frame, or any other call that takes a timestamp, fails with `ErrTimeOverflow` past `2^61 - 1` microseconds instead of `2^62 - 1`, the most moq-lite-05/06 can carry.
A received timestamp past it fails the group as a malformed track.
Clamp timestamps you build from unbounded input to `2^61 - 1`.

### Finished tracks stay until the broadcast ends (#6157)

A finished track now stays listed, with its catalog rendition and demand, until its broadcast closes.
Publish a replacement under a new name, and expect a finished track's demand watch to end with the broadcast.

### A claim answers by publishing (#5678)

`OriginProducer.Dynamic(prefix)` takes no route, and `OriginDynamic.Update` is gone.
Replace `request.Accept(broadcast)` with `request.Prepare(epoch)`, which returns a `*BroadcastPrepared`: fill its `Broadcast()`, then `Publish()` announces it at the exact path under that epoch and returns its producer.
Where you rejected a request after a failed setup, `Close()` the prepared answer instead.

### `TrackInfo` gains `Compress` (#5928)

A positional `TrackInfo{...}` literal adds a trailing `false`; keyed literals need no change.

### Anchors can name a time (#6074)

`AnchorTime` names the group containing a presentation time, and type switches over `Anchor` need a case for it.
`TrackConsumer.Update` also fails on a time anchor an untimed track can't serve.

### Rebuild the matching raw bindings

The documentation update changes UniFFI's audio-producer method metadata checksums.
Update generated `moq.dev/moq-ffi` sources and its bundled static libraries together,
then rebuild the application; older generated guards reject the rebuilt library.
Application calls are unchanged.

### JSON consumers return `*json.Timed` (#5677)

`SnapshotConsumer.Next`, `StreamConsumer.Next`, and `All` return `*Timed{Value, Timestamp}` instead of the raw message; `Timestamp` is a `*time.Duration`, nil when untimed.
A snapshot consumer's `Next` now yields every state in order; call `Latest(ctx)` for the newest.
Timestamps are rounded down to a whole microsecond.

### Ranges filter reads (#5929)

A subscription now reads only the groups its own `Ranges` cover.

### Track demand reports the wanted group (#5930)

`TrackDemand.IsUsed`, `Used`, and `Unused` are gone.
`Wanted()` returns nil while nobody subscribes, otherwise the group subscribers want next, and `WantedChanged(ctx)` waits for it to change (the first call returns at once).
Loop on `WantedChanged` until it returns non-nil where you called `Used`, and nil where you called `Unused`.
`TrackInfo` gains `Refresh`.

### `CongestionControl` gained `CongestionControlNone` (#5926)

`CongestionControlNone` runs no congestion control and no pacing, for tests and controlled links.
A `switch` that handles every value needs one more case.

### Key logging is opt-in (#5907)

`SSLKEYLOGFILE` alone no longer writes TLS session secrets.
Pass `WithQUICKeylog(true)` to `Dial` or `WithServerQUICKeylog(true)` to `Listen` as well to capture them; without `SSLKEYLOGFILE` the dial or listen fails.

### Subscriptions carry group ranges (#5732)

`Subscription.GroupStart` and `GroupEnd` are replaced by `Ranges`, a slice of `Range` whose ends are `AnchorAbsolute` or `AnchorRelative` (an absolute group and frame, or a count of groups before the latest), and an optional `Order`.
To start at group `n`, pass one range starting at the absolute anchor for group `n`, frame 0.

### Receive estimates come from `Session.RecvBandwidth` (#5777)

The connection stats no longer carry a receive estimate.
Hold the consumer `Session.RecvBandwidth` returns to request PROBE, read it with `Rate()` or wait with `RateChanged(ctx)`, and `Close()` it to stop.
It returns `nil` when the version has no PROBE.

### `AnnouncedBroadcast.Available` waits for the next publisher instance (#5700)

The first call returns the broadcast serving the path, as before.
Each later call waits for the next instance to take over the path, instead of returning whatever serves it now.

### A timed track refuses a group that rewinds (#5803)

Writing a frame fails with `TimestampMismatch` when a group's first frame starts before a lower-sequence group's, or after a higher one's.
A publisher whose timeline restarts publishes a new broadcast instead.

### Bandwidth reserves a track's demand (#5845)

`Bandwidth.Reserve` takes the track's `Demand()` instead of the producer, and no longer returns an error.

### Media subscribe and fetch take a catalog rendition (#5834)

`media.TrackConfig` and `media.GroupConfig` replace `Name` and `Container` with a `Rendition`, built by `media.VideoRendition` or `media.AudioRendition`, and follow its broadcast and track themselves.

```go
config := media.TrackConfig{Rendition: media.VideoRendition(id, video)}
```

### Native codecs have `audio` and `video` packages (#5694)

`BroadcastProducer.EncodeAudio` / `EncodeVideo` are `audio.NewProducer` / `video.NewProducer` (`moq.dev/moq/audio`, `moq.dev/moq/video`), built from a `media.CatalogProducer`.
The audio track name belongs to `ProducerOptions.Track`, and durations and timestamps are `time.Duration` (`FrameDuration`, `audio.Frame.Timestamp`, `video.Frame.Timestamp`); a zero `FrameDuration` takes the codec's own frame.
`BroadcastConsumer.DecodeAudio` / `DecodeVideo` are `audio.Subscribe` / `video.Subscribe`, taking the broadcast after the context.
Producers expose subscriber state only through `Demand()`, and a video GOP is `video.Keyframe(interval)`.
`audio.NewEncoder` / `video.NewEncoder` encode without publishing.

```go
catalog, err := moqmedia.NewCatalogProducer(broadcast)
video, err := moqvideo.NewProducer(catalog, moqvideo.ProducerOptions{Input: input, Output: output})
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

### Origins are `consume`, and servers accept with them

`WithSubscribeOrigin` is `WithConsumeOrigin`.
A server request's `SetPublish` / `SetConsume` are arguments to `Accept(ctx, publish, consume)`, where `nil` inherits the server's origin.
`Server.Requests(ctx)` is `All(ctx)`, and the `Status*` constants are `ConnectionStatus*`.
A value the native side cannot use fails `Dial` with a config error, including a bad bind address.

### Media lives in `moq.dev/moq/media`

`BroadcastProducer.PublishAudio`, `PublishVideo`, `PublishContainer`, their `OnTrack` and `Stream` variants, `SetVideoProperties`, and `SetCatalogSection`, and the consumer's catalog and media subscriptions, are gone.
Use `media.NewAudioTrackProducer` / `NewVideoTrackProducer`, `NewContainerProducer`, `NewCatalogProducer`, `NewCatalogConsumer`, `NewTrackConsumer`, and `media.FetchGroup`; `media.NewGroupConsumer` decodes a raw group.
Producers drop `Name`, `Used`, and `Unused`; read them through `Demand()`.
JSON and opaque data tracks live in `moq.dev/moq/json` and `moq.dev/moq/flate`, wrapping a track from `PublishTrack`.

```go
audio, err := media.NewAudioTrackProducer(broadcast, media.Named{}, media.AudioInit{Format: media.AudioFormatOpus, Data: opusHead})
```

### Container byte streams use `media.NewContainerProducer` (#5919)

`BroadcastProducer.PublishContainerStream` has no separate importer: a container recovers its own framing, so `media.NewContainerProducer` takes chunks split anywhere.
Leave `Data` empty and write the stream through it.
`ContainerInit`'s optional `Timebase` shares one source's offset across container imports.

```go
stream, err := media.NewContainerProducer(broadcast, media.ContainerInit{Format: media.ContainerFormatFmp4})
```

### Durations are `time.Duration`

`Frame`, `Datagram`, `Subscription`, `TrackInfo`, and `ConnectionStats` carry `time.Duration` instead of microsecond integers: `TimestampUs` is `Timestamp`, and `RttUs` is `RTT`.
A frame timestamp is a `*time.Duration`, `nil` on a frame read from an untimed track.

### Timestamps are optional

An omitted timescale now publishes a raw track untimed instead of selecting microseconds. Explicitly set a microsecond timescale to preserve the released default.

A raw write to a timed track without a timestamp fails with `TimestampMismatch` instead of going out at 0, and whole-frame media writes require one.
`media.CatalogProducer.Now()` samples the broadcast clock for local-event stamps.
Set `Timestamp` on every write to a timed track.

### Subscriber staleness is `MaxDelay`

`Subscription.MaxAgeUs` is `MaxDelay`, a `*time.Duration` whose nil sets no subscriber limit, so start from `moq.DefaultSubscription()` for the old zero default. `audio.DecoderOutput` / `video.DecoderOutput` take `MaxDelay *time.Duration`.
`TrackInfo.MaxAge` keeps its name: it is the publisher's retention.

### Error payloads are named `Reason`

Errors that carried a positional string (`Transport`, `Internal`, `Connect`, ...) name it `Reason`: read `Reason` instead of `Field0`.

### `Transport` gains `WebTransport`

A WebTransport session reports `TransportWebTransport`; `TransportQUIC` now means raw QUIC only.
Check for `TransportWebTransport` too where you matched `TransportQUIC`.

### `Stalled` is `Enabled` (#4915)

`media.Video.Stalled` is `Enabled`, defaulting to true with the meaning inverted, and `media.Audio` gains `Enabled`.
Read `Enabled` where you read `!Stalled`.

### Routes have one cost (#4661)

`Route.Cold` is removed; set only `Cost`.

### Decoded video frames are objects

`VideoDecodedFrame` is a handle with `TimestampUs()`, `Width()`, `Height()`, `Pixels(format)`, `Surface()`, and `Close()`.
`VideoDecoderOutput.Format` is gone; set `Surface: true` to keep the decoder's native surface.
Call `frame.Pixels(moq.VideoPixelFormatRgba)` instead of reading `frame.Data`, and `Close()` the frame when done.

## 0.5 -> 0.7

### The module path is `moq.dev/moq`

Import `moq.dev/moq` instead of `github.com/moq-dev/moq-go/moq`.

### Publish split by kind

`PublishMedia*` is `PublishAudio`, `PublishVideo`, or `PublishContainer`, each taking its own init.
The raw encoder paths that used to be `PublishAudio` / `PublishVideo` are `EncodeAudio` / `EncodeVideo`.

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
Match with `moq.ProtocolError(err)` instead of `errors.Is(err, moq.ErrProtocol)`, and read its `Kind` and `Code`.
