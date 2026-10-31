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
`TrackInfo.Compress` sends each group DEFLATE-compressed to opt-in `moq-lite-07-wip` peers; other peers, and every reader here, get the original frames. Set it on JSON tracks.
The JSON producers' `Update` / `Append` take a final `*time.Duration` timestamp argument (`nil` for untimed).
JSON consumers return a `*Timed` holding the value and that timestamp; `SnapshotConsumer.Latest`
skips to the newest state where `Next` yields each one. Read timestamps are rounded down to a whole microsecond.
Whole-frame media writes require caller time; container imports keep their own timeline handling.
Timed raw/data tracks require caller-supplied timestamps. Use `CatalogProducer.Now()` to stamp a locally
created event on the broadcast clock; reading it fixes that clock for later media producers.
Share `media.NewTimebase(broadcast)` through `ContainerInit` when several imports use one
source timeline; omitting it keeps an independent offset. Timebases belong to one broadcast
and do not keep it open; release the handle with `Close` on the calling goroutine.
`NewContainerProducer` takes chunks split anywhere, so a raw byte stream needs no other importer.
`ContainerFormatFmp4` splits each fragment into one frame per sample.
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
client, err := moq.Dial(ctx, "https://relay.example.com", moq.ClientConfig{
    TLS: moq.ClientTLS{Roots: []string{"ca.pem"}},
})
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
    case moq.AnnounceEventTakeover: // continues what an earlier route served, like a start
        ann = event.Announce
    default:
        continue // AnnounceEventUpdate or AnnounceEventEnd
    }
    // Prefix stays origin-relative; Captures reports what each wildcard matched.
    fmt.Printf("captures: %v\n", ann.Captures)
    broadcast, err := client.Request(ctx, ann.Prefix, nil)
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
import (
    "time"
    moqvideo "moq.dev/moq/video"
)

// Publish encoded frames, or raw pixels with the codec inside the binding.
// opusInit, packet, pts, and rgba come from your encoder or capture source.
publication, _ := client.CreateBroadcast("my-stream.hang")
broadcast := publication.Broadcast()
audio, _ := moqmedia.NewAudioTrackProducer(broadcast, moqmedia.Named{}, moqmedia.AudioInit{Format: moqmedia.AudioFormatOpus, Data: opusInit})
timestamp := 20 * time.Millisecond
_ = audio.WriteFrame(moq.Frame{Payload: packet, Timestamp: &timestamp})
_ = audio.Cut() // audio has no keyframes, so this is what gives it groups

track := "camera"
catalogProducer, _ := moqmedia.NewCatalogProducer(broadcast)
video, _ := moqvideo.NewProducer(catalogProducer, moqvideo.ProducerOptions{
    Input: moqvideo.EncoderInput{Format: moq.VideoPixelFormatRgba, Width: 1280, Height: 720, Framerate: moqvideo.FrameRate{Numerator: 30000, Denominator: 1001}},
    Output: moqvideo.EncoderOutput{Codec: moq.VideoCodecH264, Kind: moq.AutoEncoder()},
    Track: &track,
})
_ = video.Write(moqvideo.Frame{Timestamp: time.Duration(pts) * time.Microsecond, Data: rgba})
epoch := moq.MintEpoch() // a fresh epoch per run
_ = publication.Announce(moq.AnnounceRoute{Epoch: &epoch})
_ = audio.Finish()
_ = video.Finish()
broadcast.Close()    // keep the producer reachable while publishing, then close explicitly
```

Raw video encoders take an exact numerator/denominator frame rate, such as 30000/1001. Producer options hold input, codec settings, track naming, and optional session bandwidth ownership; standalone encoders take only codec settings. For simulcast, give each rendition the next one down as `Lower`: it stops encoding, advertised as disabled, while its bandwidth grant cannot fund it, and resumes once the grant recovers. A rung only disables while the session probes for bandwidth, which on native transports takes moq-quic with BBR3.

## Things to know

The rest of the [shared feature list](/lib/#what-every-binding-can-do) maps
one to one; the API reference has the names, and [Layers](/lib/layers)
shows which module holds each.

- **Context cancellation.** Every call that can block takes a `context.Context` first, and cancelling it tears down the native work, not just your wait. A one-shot call (`Request`, `FetchGroup`, `Server.Accept`) aborts alone; a stream read (`Next`, `ReadFrame`, and the iterators over them) cancels the stream it reads.
- **Keep owners reachable.** An origin from `moq.NewOriginProducer` has no `Close`. It ends when the garbage collector reaches its last owner (each producer, published broadcast, and `OriginRequests`), and its consumers then fail with `moq.ErrClosed`.
- **Bandwidth ownership.** An encoder you manage can reserve a share of the session's bandwidth. Reuse its reservation when the ceiling changes; before abandoning or replacing it, call `Update(0)` to return its share and automatic probe ceiling without waiting for garbage collection. The track can stay subscribed and the reservation can be reused.
- **Audio needs cuts.** Video groups at its keyframes, but audio forms a group only where you call `Cut()`: after every frame, or at a segment cadence to align with video.
- **Timeline breaks.** On a seek or pause, call `Discontinuity()`, then keep timestamps moving forward and resume video on a keyframe.
- **Media reads.** `media.TrackConsumer`, `audio.Consumer`, and `video.Consumer` return a `Read` in order: `ReadFrame`, a `ReadGap` where media was lost, or a `ReadEnd` where the publisher stopped its encoder. A gap breaks continuity. An encoder end is not stream EOF: following frames before its endpoint are the old encoder's drain; the first at or past it starts fresh.
- **Decoded frames hold decoder buffers.** `Close` each `VideoDecodedFrame` promptly, or the decoder stalls. `Resize` is best effort, so read each frame's `Width()` and `Height()`.
- **Closing.** `client.Close()` drains for up to one second and returns an error on failure. `session.Shutdown()` requests that drain immediately; `session.Closed(ctx)` waits for its result, and `session.Cancel(code)` closes at once. Finish or abort live tracks first. Application delivery requires explicitly selecting `moq-lite-07-wip` on both peers; older Lite versions (including default lite-06) and IETF wait only for transport ACKs.
- **Ending cleanly.** When the input ends, finish every track, then call `Finish()` on its `media.CatalogProducer` to finish the catalog, which ends the content. The broadcast stays announced and keeps serving the finished tracks, so a subscriber still catching up can drain them, until `broadcast.Close()` takes it offline. How long to keep it up is yours to decide. A track still open at the close, or released without a finish, ends with `Unroutable`, which readers take as the publisher leaving.
- **Stats.** `Session().Stats()` reports `RTT`, `EstimatedSendRateBps`, and the byte and packet counters (`BytesSent`, `BytesReceived`, `BytesLost`, `PacketsSent`, `PacketsReceived`, `PacketsLost`). A field is nil when the transport does not report it, which is not the same as zero. `Version` names the negotiated protocol. `Dial` returns a connecting session; read `Session().State()` and wait with `StateChanged(ctx, state.Revision)` to observe first-dial failures and reconnects. Hold the owned `BandwidthConsumer` returned by `Session().RecvBandwidth()` to receive bandwidth estimates. `Rate()` reads the current estimate in bits per second and `RateChanged(ctx)` waits for a new one. Calling `Close()` on the last consumer stops PROBE; the session and stats snapshots do not request it. Client consumers survive reconnects and report no estimate while disconnected.
- **Authorization.** On `moq-lite-07-wip` (opt in with `ClientConfig.Versions`) and on moq-transport draft-18+ with the [MoQ Auth extension](/draft/moq-auth), `Session().Grants()` returns a `GrantConsumer`: `Grant()` is the `Grant` the relay sent back, nil until it answers and while a client session reconnects, and `Changed(ctx)` waits for the next one. `Close()` it when done.
- **Tokens.** Set `ClientConfig.Tokens` instead of putting the credential in the URL. The first is the connection credential, sent in SETUP and, while an offered version can't carry it there (lite before 07), as the URL's `?jwt=`; each other token widens the grant over AUTH. `Session().Tokens().Set(...)` replaces a client session's tokens before they expire: the next dial presents them, a live AUTH session at once, and after an authorization refusal the session redials only if they changed. `Answered(ctx)` returns nil once the relay accepts them, or its refusal with the relay's code. A `?jwt=` already in the URL stays the connection credential, which is how to reach a relay older than this release.
- **Errors.** Read diagnostic payloads from `Reason` and structured protocol failures from `Details`.
- **Disabled renditions.** A rendition with `enabled: false` has no frames coming until its publisher re-enables it. Select an enabled rendition from the latest catalog before opening a decoder. Keep one already playing while every rendition of its kind is disabled: the convenience decoders accept a disabled configuration, and a re-enable, such as an unmute, then plays without a resubscribe. Among the video renditions you can decode, prefer the highest `Preference`; `0` is the default and `-1` marks a compatibility transcode.
- **Subscription defaults.** `moq.DefaultSubscription()` returns moq-net's defaults, whose zero `MaxDelay` skips a superseded group at once; a nil `*Subscription` means the same. Change its fields rather than starting from `Subscription{}`, whose nil `MaxDelay` sets no subscriber limit.
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
A session closed because its credential expired reports `ProtocolKindExpired`, and a request refused for an expired token raises `moq.ErrExpired`; refresh the token before retrying. Malformed credentials report `ProtocolKindMalformedAuthToken`; correct the token before reconnecting with it.
A catalog that needs a newer moq-hang draft than this build reads raises `moq.ErrUnsupportedCatalog`; upgrade the library to read it.

## Reference

- API reference: [pkg.go.dev/moq.dev/moq](https://pkg.go.dev/moq.dev/moq)
- Source: [`go/`](https://github.com/moq-dev/moq/tree/main/go); `just go check` builds and tests locally
- Mirrors the vanity path resolves to: [moq-dev/moq-go](https://github.com/moq-dev/moq-go) (wrapper), [moq-dev/moq-go-ffi](https://github.com/moq-dev/moq-go-ffi) (raw bindings and static libraries)
