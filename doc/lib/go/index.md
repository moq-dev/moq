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
import "moq.dev/moq"

// Subscribe. The iterator is live, so run it in its own goroutine.
client, err := moq.Dial(ctx, "https://relay.example.com", moq.WithTLSRoots("ca.pem"))
if err != nil {
    log.Fatal(err)
}
defer client.Close()

announced, err := client.Announced("live/")
if err != nil {
    log.Fatal(err)
}
for ann, err := range announced.All(ctx) {
    if err != nil {
        if moq.IsShutdown(err) { break }
        log.Fatal(err)
    }
    // An announcement is a route; resolve the broadcast at its path.
    broadcast, err := client.RequestBroadcast(ctx, "live/" + ann.Path())
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
_ = audio.WriteFrame(moq.Frame{Payload: packet, TimestampUs: 20_000})

track := "camera"
video, _ := broadcast.EncodeVideo(
    moq.VideoEncoderInput{Format: moq.VideoPixelFormatRgba, Width: 1280, Height: 720, Framerate: 30},
    moq.VideoEncoderOutput{Codec: moq.VideoCodecH264, Track: &track, Kind: moq.AutoEncoder()},
)
_ = video.Write(moq.VideoFrame{TimestampUs: pts, Data: rgba})
broadcast.Finish()   // keep the producer reachable while publishing, then finish explicitly
```

Every call that can block takes a `context.Context` first. Cancelling it
returns `ctx.Err()` promptly and tears the in-flight native work down, so a
per-call deadline bounds resource use rather than just your wait. What it tears
down depends on the call: a one-shot (`SubscribeTrack`, `FetchGroup`,
`RequestBroadcast`, `Server.Accept`, ...) aborts alone and leaves its object
usable, while a stream read (`Next`, `RecvGroup`, `ReadFrame`, and the
`iter.Seq2` iterators over them) cancels the stream it reads, which is what a
range loop over a cancelled context wants.

`moq.Listen` accepts sessions with per-request `Accept`/`Reject`. JSON tracks
take anything `encoding/json` handles and return `json.RawMessage`. The rest
of the [shared feature list](/lib/#what-every-binding-can-do) maps one to
one: `FetchGroup`/`FetchMediaGroup`, `Dynamic()` with `Requests(ctx)`,
`AppendDatagram`/`Datagrams(ctx)`, `SetCatalogSection`, `Used`/`Unused`,
`Session().Stats()`. `moq.IsAuthError` and `moq.IsShutdown` classify errors.

- API reference: [pkg.go.dev/moq.dev/moq](https://pkg.go.dev/moq.dev/moq)
- Source: [`go/`](https://github.com/moq-dev/moq/tree/main/go); `just go check` builds and tests locally
- Mirrors the vanity path resolves to: [moq-dev/moq-go](https://github.com/moq-dev/moq-go) (wrapper), [moq-dev/moq-go-ffi](https://github.com/moq-dev/moq-go-ffi) (raw bindings and static libraries)
