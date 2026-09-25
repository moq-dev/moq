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

filter := "*/camera"
announced, err := client.Announced(moq.AnnounceOptions{Prefix: "live/", Filter: &filter})
if err != nil {
    log.Fatal(err)
}
for ann, err := range announced.All(ctx) {
    if err != nil {
        if moq.IsShutdown(err) { break }
        log.Fatal(err)
    }
    // Updates stay origin-relative; Captures reports what each wildcard matched.
    fmt.Printf("captures: %v\n", ann.Captures())
    broadcast, err := client.RequestBroadcast(ctx, ann.Prefix())
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
    nil,
)
_ = video.Write(moq.VideoFrame{TimestampUs: pts, Data: rgba})
_ = broadcast.Announce(moq.Route{})
broadcast.Finish()   // keep the producer reachable while publishing, then finish explicitly
```

The three advertising operations: `client.CreateBroadcast(path)` (or
`origin.CreateBroadcast`) returns an unannounced producer, invisible to everyone;
`broadcast.Announce(route)` / `broadcast.Unannounce()` own that exact-path
advertisement; `origin.Dynamic(prefix, route)` claims `prefix` and every
path beneath it (`""` for everything). Hold the returned `OriginDynamic`
while the claim should stay advertised, and reject the requests you will not
serve. A route is a capability, not an inventory. `Announced(options)` combines
a literal prefix with an optional relative pattern; `ann.Prefix()` stays
relative to the origin and `ann.Captures()` reports the wildcard matches.
Paths with a `.`-prefixed segment below the prefix are [hidden](/concept/moq-lite#hidden-broadcasts) unless
`Hidden: true`.

Every call that can block takes a `context.Context` first. Cancelling it
returns `ctx.Err()` promptly and tears the in-flight native work down, so a
per-call deadline bounds resource use rather than just your wait. What it tears
down depends on the call: a one-shot (`SubscribeTrack`, `FetchGroup`,
`RequestBroadcast`, `Server.Accept`, ...) aborts alone and leaves its object
usable, while a stream read (`Next`, `RecvGroup`, `ReadFrame`, and the
`iter.Seq2` iterators over them) cancels the stream it reads, which is what a
range loop over a cancelled context wants.

Sessions reconnect with backoff when the transport drops and re-announce local
broadcasts, so a worker rides out a relay restart. `Session().Epoch()` counts
the connections, 1 on the first, pairing with `Session().Status(ctx)` to log
each reconnect by number; `moq.WithBackoff` tunes the pacing, with
`moq.RetryForever` as the timeout; and `moq.WithQUICMaxStreams` raises the
peer's inbound stream cap for a subscriber to many tracks.

The [WebSocket fallback](/concept/transport#websocket-fallback) races QUIC after
a 200 ms head start. `moq.WithWebSocketEnabled(false)` turns it off for a
QUIC-only relay, and `moq.WithWebSocketDelay` changes the head start.

`moq.Listen` accepts sessions with per-request `Accept`/`Reject`; `Request.Transport()`
returns the closed `moq.Transport` enum.
`Request.SetPublish`/`SetConsume` return an error if the request is already
answered, cancelled, or currently accepting; `ErrBusy` is the race with an
in-flight Accept. JSON tracks
take anything `encoding/json` handles and return `json.RawMessage`. The rest
of the [shared feature list](/lib/#what-every-binding-can-do) maps one to
one: `FetchGroup`/`FetchMediaGroup`, `Dynamic()` with `Requests(ctx)`,
`Session.Bandwidth()` to divide the send estimate,
`AppendDatagram`/`Datagrams(ctx)`, `SetCatalogSection`, `Demand()` for `Used`/`Unused`,
`Session().Stats()`. `moq.IsAuthError` and `moq.IsShutdown` classify errors. `moq.ProtocolError(err)` is the structured protocol failure (scope, verbatim code, kind) when the peer sent one.

`DecodeVideo` picks the decoded CPU pixel layout: `VideoDecoderOutput.Format`
is I420 when nil, or `VideoPixelFormatRgba` for four bytes a pixel, and every
`VideoDecodedFrame` repeats the layout it was decoded to. `Resize` is best
effort: only NVDEC has a built-in scaler, so read each frame's own `Width` and
`Height` rather than assuming it took.

## Connection stats

`Session().Stats()` returns a `ConnectionStats` snapshot. Each field is a
pointer, nil when the transport backend does not report it (native QUIC reports
all of them; browser WebTransport reports few or none) or before it is
available, which is not the same as zero.

| Field | Unit | Meaning |
| --- | --- | --- |
| `RttUs` | microseconds | Smoothed round-trip time. |
| `EstimatedSendRateBps` | bits per second | Send bandwidth from the congestion controller. |
| `EstimatedRecvRateBps` | bits per second | Receive bandwidth from MoQ PROBE. |
| `BytesSent` | bytes | Total sent, including retransmissions and overhead. |
| `BytesReceived` | bytes | Total received, including duplicates and overhead. |
| `BytesLost` | bytes | Total lost, detected via retransmission or acknowledgement. |
| `PacketsSent` | datagrams | Total datagrams sent. |
| `PacketsReceived` | datagrams | Total datagrams received. |
| `PacketsLost` | datagrams | Total datagrams detected as lost. |

- API reference: [pkg.go.dev/moq.dev/moq](https://pkg.go.dev/moq.dev/moq)
- Source: [`go/`](https://github.com/moq-dev/moq/tree/main/go); `just go check` builds and tests locally
- Mirrors the vanity path resolves to: [moq-dev/moq-go](https://github.com/moq-dev/moq-go) (wrapper), [moq-dev/moq-go-ffi](https://github.com/moq-dev/moq-go-ffi) (raw bindings and static libraries)
