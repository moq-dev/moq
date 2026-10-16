---
title: Go
description: Idiomatic Go over cgo via moq.dev/moq
---

# Go

[![Go Reference](https://pkg.go.dev/badge/moq.dev/moq.svg)](https://pkg.go.dev/moq.dev/moq)

`moq.dev/moq`: `context.Context` cancellation, `error`
returns, and Go 1.23 range-over-func iterators for live streams. The native
core arrives as a prebuilt static library through the `moq.dev/moq-ffi` module, so
`go get` is all it takes (`CGO_ENABLED=1`, the default on Unix). Targets:
linux/amd64, linux/arm64, darwin/arm64 (macOS 12.3+), windows/amd64.

`moq.dev/moq/media` owns catalogs, encoded imports, and container consumers.
Construct `NewAudioTrackProducer` / `NewVideoTrackProducer` from a broadcast, an init record,
and a `Named` or `Requested` target. `NewCatalogProducer` holds its broadcast weakly;
catalog writes fail with `ErrClosed` after the broadcast closes. Media timestamps use `time.Duration`.
A missing data timestamp stays untimed; the library never substitutes zero or now.
Raw tracks with a `nil` timescale are untimed on both publish and receive.
`moq.dev/moq/flate` publishes opaque snapshot and stream payloads; its `Update` / `Append`
and the JSON producers take a final `*time.Duration` timestamp argument (`nil` for untimed).
Whole-frame media writes require caller time; container imports keep their own timeline handling.
Timed raw/data tracks require caller-supplied timestamps. Use `CatalogProducer.Now()` to stamp a locally
created event on the broadcast clock; reading it fixes that clock for later media producers.
Share `media.NewTimebase(broadcast)` through container init records when several imports
use one source timeline. `NewContainerStreamProducer` takes `ContainerStreamInit`;
omitting its timebase keeps an independent offset. Timebases belong to one broadcast
and do not keep it open; release the handle with `Close` on the calling goroutine.
For arrival-order reads, decode each group from `moq.TrackConsumer.RecvGroup` with
`media.NewGroupConsumer`; frames carry container media time as they land. `media.FetchGroup`
fetches and decodes one past group.

```bash
go get moq.dev/moq@latest
```

```go
import "fmt"
import "log"
import "moq.dev/moq"
import moqmedia "moq.dev/moq/media"

// Subscribe. The iterator is live, so run it in its own goroutine.
client, err := moq.Dial(ctx, "https://relay.example.com", moq.WithTLSRoots("ca.pem"))
if err != nil {
    log.Fatal(err)
}
defer client.Close()

filter := "*/camera"
announced, err := client.Announced(moq.AnnounceOptions{Prefix: "live/", Filter: &filter})
if err != nil {
    log.Fatal(err)
}
for event, err := range announced.All(ctx) {
    if err != nil {
        if moq.IsShutdown(err) { break }
        log.Fatal(err)
    }
    var ann moq.Announce
    switch event := event.(type) {
    case moq.AnnounceEventStart:
        ann = event.Announce
    case moq.AnnounceEventRestart: // a new publisher run at the same path: request it again
        ann = event.Announce
    default:
        continue // AnnounceEventUpdate or AnnounceEventEnd
    }
    // Prefix stays origin-relative; Captures reports what each wildcard matched.
    fmt.Printf("captures: %v\n", ann.Captures)
    broadcast, err := client.RequestBroadcast(ctx, ann.Prefix)
    if err != nil {
        log.Fatal(err)
    }
    catalog, err := moqmedia.CatalogSnapshot(ctx, broadcast)
    if err != nil {
        log.Fatal(err)
    }
    fmt.Printf("%+v\n", catalog)
}
```

```go
import "time"

// Publish encoded frames, or raw pixels with the codec inside the binding.
// opusInit, packet, pts, and rgba come from your encoder or capture source.
broadcast, _ := client.CreateBroadcast("my-stream.hang")
audio, _ := moqmedia.NewAudioTrackProducer(broadcast, moqmedia.Named{}, moqmedia.AudioInit{Format: moqmedia.AudioFormatOpus, Data: opusInit})
timestamp := 20 * time.Millisecond
_ = audio.WriteFrame(moq.Frame{Payload: packet, Timestamp: &timestamp})
_ = audio.Cut() // audio has no keyframes, so this is what gives it groups

track := "camera"
video, _ := broadcast.EncodeVideo(
    moq.VideoEncoderInput{Format: moq.VideoPixelFormatRgba, Width: 1280, Height: 720, Framerate: 30},
    moq.VideoEncoderOutput{Codec: moq.VideoCodecH264, Track: &track, Kind: moq.AutoEncoder()},
    nil,
)
_ = video.Write(moq.VideoFrame{TimestampUs: pts, Data: rgba})
epoch := moq.MintEpoch() // a fresh epoch per run
_ = broadcast.Announce(moq.Route{Epoch: &epoch})
_ = audio.Finish()
_ = video.Finish()
broadcast.Close()    // keep the producer reachable while publishing, then close explicitly
```

## Things to know

The rest of the [shared feature list](/lib/#what-every-binding-can-do) maps
one to one; the API reference has the names.

- **Context cancellation.** Every call that can block takes a `context.Context` first, and cancelling it tears down the native work, not just your wait. A one-shot call (`RequestBroadcast`, `FetchGroup`, `Server.Accept`) aborts alone; a stream read (`Next`, `ReadFrame`, and the iterators over them) cancels the stream it reads.
- **Keep owners reachable.** An origin from `moq.NewOriginProducer` has no `Close`. It ends when the garbage collector reaches its last owner (each producer, published broadcast, and `OriginDynamic`), and its consumers then fail with `moq.ErrClosed`.
- **Audio needs cuts.** Video groups at its keyframes, but audio forms a group only where you call `Cut()`: after every frame, or at a segment cadence to align with video.
- **Live encoder timing.** After writing a frame you encoded yourself, call `Flush(timestamp)` with the same timestamp so the catalog advertises your jitter. Skip it for file and network imports. On a seek or pause, call `Discontinuity()`, then keep timestamps moving forward and resume video on a keyframe.
- **Decoded frames hold decoder buffers.** `Close` each `VideoDecodedFrame` promptly, or the decoder stalls. `Resize` is best effort, so read each frame's `Width()` and `Height()`.
- **Closing.** `client.Close()` drains for up to one second and returns an error on failure. `session.Shutdown(ctx)` does the same, and `session.Cancel(code)` closes at once. Finish or abort live tracks first. Application delivery requires explicitly selecting `moq-lite-07-wip` on both peers; older Lite versions (including default lite-06) and IETF wait only for transport ACKs.
- **Stats.** `Session().Stats()` reports `RTT`, `EstimatedSendRateBps`, `EstimatedRecvRateBps`, and the byte and packet counters (`BytesSent`, `BytesReceived`, `BytesLost`, `PacketsSent`, `PacketsReceived`, `PacketsLost`). A field is nil when the transport does not report it, which is not the same as zero.
- **Errors.** Read diagnostic payloads from `Reason` and structured protocol failures from `Details`.
- **Disabled renditions.** Audio and video convenience decoders reject a catalog configuration with `enabled: false`. Select an enabled rendition from the latest catalog before opening a decoder.
- **Track demand.** Obtain a dynamic track request’s `Demand()` before accepting it to watch subscribers through the handoff to its producer. The watch does not keep the track alive and ends when the track closes or the request is rejected.

## Runtime

Native I/O and background tasks run on a Tokio worker pool. Calls awaited by the
host still poll in place; independent native wake callbacks can run concurrently.
The generated bridge synchronizes wakeups before resuming the awaiting goroutine.
Application goroutines remain responsible for serializing their own shared state.
A panic in a native background task reaches the pending and later fallible calls
of every object that depends on it as a task error; other objects keep running.

A session closed for exceeding the relay's request caps reports the protocol
kind `ProtocolKindTooManyRequests`. Reduce
the simultaneous announcements or subscriptions, or ask the relay operator
to adjust the [session limits](/bin/relay/config#session-and-peer), before
reconnecting with the same workload.
A session closed because its credential expired reports `ProtocolKindExpired`, and a request refused for an expired token raises `moq.ErrExpired`; refresh the token before retrying.

## Reference

- API reference: [pkg.go.dev/moq.dev/moq](https://pkg.go.dev/moq.dev/moq)
- Source: [`go/`](https://github.com/moq-dev/moq/tree/main/go); `just go check` builds and tests locally
- Mirrors the vanity path resolves to: [moq-dev/moq-go](https://github.com/moq-dev/moq-go) (wrapper), [moq-dev/moq-go-ffi](https://github.com/moq-dev/moq-go-ffi) (raw bindings and static libraries)
