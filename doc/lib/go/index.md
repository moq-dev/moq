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

```bash
go get moq.dev/moq@latest
```

```go
import "fmt"
import "log"
import "moq.dev/moq"

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
    ann, ok := event.(moq.AnnounceEventStart)
    if !ok {
        continue // AnnounceEventUpdate or AnnounceEventEnd
    }
    // Prefix stays origin-relative; Captures reports what each wildcard matched.
    fmt.Printf("captures: %v\n", ann.Announce.Captures)
    broadcast, err := client.RequestBroadcast(ctx, ann.Announce.Prefix)
    if err != nil {
        log.Fatal(err)
    }
    catalog, err := broadcast.Catalog(ctx)
    if err != nil {
        log.Fatal(err)
    }
    fmt.Printf("%+v\n", catalog)
}
```

```go
// Publish encoded frames, or raw pixels with the codec inside the binding.
// opusInit, packet, pts, and rgba come from your encoder or capture source.
broadcast, _ := client.CreateBroadcast("my-stream.hang")
audio, _ := broadcast.PublishAudio(moq.AudioFormatOpus, opusInit)
pts := uint64(20_000)
_ = audio.WriteFrame(moq.Frame{Payload: packet, TimestampUs: &pts})
_ = audio.Cut() // audio has no keyframes, so this is what gives it groups

track := "camera"
video, _ := broadcast.EncodeVideo(
    moq.VideoEncoderInput{Format: moq.VideoPixelFormatRgba, Width: 1280, Height: 720, Framerate: 30},
    moq.VideoEncoderOutput{Codec: moq.VideoCodecH264, Track: &track, Kind: moq.AutoEncoder()},
    nil,
)
_ = video.Write(moq.VideoFrame{TimestampUs: pts, Data: rgba})
_ = broadcast.Announce(moq.Route{})
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
- **Live encoder timing.** After writing a frame you encoded yourself, call `Flush(timestampUs)` with the same timestamp so the catalog advertises your jitter. Skip it for file and network imports. On a seek or pause, call `Discontinuity()`, then keep timestamps moving forward and resume video on a keyframe.
- **Decoded frames hold decoder buffers.** `Close` each `VideoDecodedFrame` promptly, or the decoder stalls. `Resize` is best effort, so read each frame's `Width()` and `Height()`.
- **Closing.** `client.Close()` gives finished tracks up to one second to deliver and returns an error if they did not. `session.Shutdown(ctx)` does the same, and `session.Cancel(code)` closes at once. Finish or abort live tracks first.
- **Stats.** `Session().Stats()` reports `RttUs`, `EstimatedSendRateBps`, `EstimatedRecvRateBps`, and the byte and packet counters (`BytesSent`, `BytesReceived`, `BytesLost`, `PacketsSent`, `PacketsReceived`, `PacketsLost`). A field is nil when the transport does not report it, which is not the same as zero.

## Reference

- API reference: [pkg.go.dev/moq.dev/moq](https://pkg.go.dev/moq.dev/moq)
- Source: [`go/`](https://github.com/moq-dev/moq/tree/main/go); `just go check` builds and tests locally
- Mirrors the vanity path resolves to: [moq-dev/moq-go](https://github.com/moq-dev/moq-go) (wrapper), [moq-dev/moq-go-ffi](https://github.com/moq-dev/moq-go-ffi) (raw bindings and static libraries)
