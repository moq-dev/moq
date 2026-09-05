---
title: Go
description: Idiomatic Go over cgo via github.com/moq-dev/moq-go
---

# Go

[![Go Reference](https://pkg.go.dev/badge/github.com/moq-dev/moq-go.svg)](https://pkg.go.dev/github.com/moq-dev/moq-go)

`github.com/moq-dev/moq-go/moq`: `context.Context` cancellation, `error`
returns, and Go 1.23 range-over-func iterators for live streams. The native
core arrives as a prebuilt static library through the `moq-go-ffi` module, so
`go get` is all it takes (`CGO_ENABLED=1`, the default on Unix). Targets:
linux/amd64, linux/arm64, darwin/arm64 (macOS 12.3+), windows/amd64.

```bash
go get github.com/moq-dev/moq-go@latest
```

```go
import "github.com/moq-dev/moq-go/moq"

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
    catalog, err := ann.Broadcast().Catalog(ctx)
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
audio, _ := broadcast.PublishMedia("opus", opusInit)
_ = audio.WriteFrame(moq.Frame{Payload: packet, TimestampUs: 20_000})

track := "camera"
video, _ := broadcast.PublishVideo(
    moq.VideoEncoderInput{Format: moq.VideoPixelFormatRgba, Width: 1280, Height: 720, Framerate: 30},
    moq.VideoEncoderOutput{Codec: moq.VideoCodecH264, Track: &track, Kind: moq.AutoEncoder()},
)
_ = video.Write(moq.VideoFrame{TimestampUs: pts, Data: rgba})
broadcast.Finish()   // keep the producer reachable while publishing, then finish explicitly
```

`moq.Listen` accepts sessions with per-request `Accept`/`Reject`. JSON tracks
take anything `encoding/json` handles and return `json.RawMessage`. The rest
of the [shared feature list](/lib/#what-every-binding-can-do) maps one to
one: `FetchGroup`/`FetchMediaGroup`, `Dynamic()` with `Requests(ctx)`,
`AppendDatagram`/`Datagrams(ctx)`, `SetCatalogSection`, `Used`/`Unused`,
`Session().Stats()`. `moq.IsAuthError` and `moq.IsShutdown` classify errors.

- API reference: [pkg.go.dev/github.com/moq-dev/moq-go](https://pkg.go.dev/github.com/moq-dev/moq-go)
- Source: [`go/`](https://github.com/moq-dev/moq/tree/main/go); `just go check` builds and tests locally
- Modules `go get` resolves: [moq-dev/moq-go](https://github.com/moq-dev/moq-go) (wrapper), [moq-dev/moq-go-ffi](https://github.com/moq-dev/moq-go-ffi) (raw bindings and static libraries)
