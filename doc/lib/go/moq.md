---
title: moq (Go)
description: Ergonomic Go module for Media over QUIC
---

# moq

The ergonomic Go module for [Media over QUIC](/). This is the package most callers want.

It wraps the raw [moq-ffi](/lib/go/moq-ffi) bindings in idiomatic Go: `context.Context` cancellation, Go `error` returns (with an `IsShutdown` helper for graceful Cancelled/Closed), and Go 1.23 range-over-func iterators (`iter.Seq2`) for live streams. The record and enum types are re-exported without the `Moq` prefix, so most programs never import the ffi package directly.

## Install

```bash
go get github.com/moq-dev/moq-go@latest
```

```go
import "github.com/moq-dev/moq-go/moq"
```

`CGO_ENABLED=1` is required (the default on Unix). The prebuilt `libmoq_ffi.a` comes transitively from [moq-ffi](/lib/go/moq-ffi), which the wrapper requires; cgo selects the right archive automatically, so there's no Rust toolchain or shared-library setup.

Go's minimum-version-selection resolves to the maximum `moq-ffi` across your build graph, and CI re-publishes the wrapper with its `require` bumped to the newest `moq-ffi` on every release, so `@latest` always pulls the latest native core.

## Quick start

```go
ctx := context.Background()

client, err := moq.Dial(ctx, "https://relay.example.com")
if err != nil {
	log.Fatal(err)
}
defer client.Close()

announced, err := client.Announced("demos/")
if err != nil {
	log.Fatal(err)
}
for ann, err := range announced.All(ctx) {
	if err != nil {
		if moq.IsShutdown(err) {
			break
		}
		log.Fatal(err)
	}
	fmt.Println("got broadcast", ann.Path())

	catalog, err := ann.Broadcast().Catalog(ctx)
	if err != nil {
		log.Fatal(err)
	}
	fmt.Printf("catalog: %+v\n", catalog)
}
```

## TLS and stats

Use certificate roots or fingerprints when a client needs to trust a private or
self-signed endpoint without disabling verification:

```go
client, err := moq.Dial(ctx, "https://relay.example.com",
    moq.WithTLSRoots("/etc/ssl/custom-ca.pem"),
    moq.WithTLSSystemRoots(true),
    moq.WithTLSFingerprints("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"),
)
if err != nil {
    log.Fatal(err)
}
defer client.Close()

stats := client.Session().Stats()
fmt.Printf("rtt: %v\n", stats.RttUs)
```

`Stats()` returns a snapshot. Individual fields are nil when the transport does
not report that metric yet.

## Dynamic tracks

`BroadcastProducer.Dynamic()` lets a publisher accept tracks that subscribers
request before they exist:

```go
broadcast, err := moq.NewBroadcastProducer()
if err != nil {
    log.Fatal(err)
}
defer broadcast.Finish()

dynamic, err := broadcast.Dynamic()
if err != nil {
    log.Fatal(err)
}
defer dynamic.Cancel()

go func() {
    request, err := dynamic.RequestedTrack(ctx)
    if err != nil {
        return
    }
    track, err := request.Accept(nil)
    if err != nil {
        return
    }
    _ = track.WriteFrame(moq.Frame{Payload: []byte("ready")})
}()
```

For media tracks, let the importer accept the request:

```go
request, err := dynamic.RequestedTrack(ctx)
if err != nil {
    log.Fatal(err)
}
media, err := broadcast.PublishMediaOnTrack(request, "opus", opusInit)
if err != nil {
    log.Fatal(err)
}
_ = media.WriteFrame(moq.Frame{Payload: opusFrame, TimestampUs: 20_000})
```

Video catalog fields that are known before the first keyframe can be supplied
with `WithVideoHint`:

```go
media, err := broadcast.PublishMedia("avc3", nil, moq.WithVideoHint(moq.VideoHint{
    Coded: &moq.Dimensions{Width: 1920, Height: 1080},
}))
```

## Error handling

A server can reject the connection on auth grounds: `ErrMoqErrorUnauthorized` (HTTP 401) or `ErrMoqErrorForbidden` (HTTP 403). These are terminal: retrying without new credentials won't help, so handle them separately from a transient transport failure. The `moq.IsAuthError` helper catches both:

```go
session, err := client.Connect("https://relay.example.com")
if moq.IsAuthError(err) {
    // Prompt for credentials; don't reconnect.
}
```

## Publishing lifetime

A broadcast stays live only while you hold its `BroadcastProducer`. Once the producer is garbage-collected the path unannounces and subscribers get a reset mid-stream. This bites when the producer goes out of scope while a background goroutine is still writing to its tracks.

Keep a reference for as long as you are publishing, then close it explicitly when done:

```go
broadcast, err := origin.CreateBroadcast("my-broadcast.hang")
if err != nil {
    // handle error
}
mediaProducer, err := broadcast.PublishMedia("aac", asc)
if err != nil {
    // handle error
}

// Keep `broadcast` reachable while producing (e.g. store it on a struct the
// publishing goroutine owns). Don't let it fall out of scope here.
produceAudio(mediaProducer)

// Finish() closes the broadcast cleanly and unpublishes it immediately,
// so subscribers see a normal end.
broadcast.Finish()
```

If a producer is collected without `Finish()`, the underlying library logs a warning (`broadcast::Producer dropped without close()`) to help you spot the leak.

## Raw Track Controls

Raw track subscribers can query the publisher's track properties and change their own delivery preferences without resubscribing:

```go
subscription := moq.Subscription{Priority: 10, Ordered: true}
track, err := broadcast.SubscribeTrack("events", &subscription)
if err != nil {
	log.Fatal(err)
}

info, err := track.Info()
if err != nil {
	log.Fatal(err)
}
if info.Timescale != nil {
	fmt.Println("timescale", *info.Timescale)
}

track.Update(moq.Subscription{Priority: 20, Ordered: false})
```

`Ordered` controls prioritization only. When true, groups are prioritized in sequence order. Groups may always arrive out-of-order (or not at all) over the network.

For sparse or replayed tracks, use `CreateGroup(sequence)`. `FinishAt(finalSequence)` declares the exclusive end while still permitting lower groups, and `Abort(errorCode)` terminates a track or group with an application error.

## JSON tracks

Use `PublishJSONSnapshot` / `SubscribeJSONSnapshot` for lossy latest state and `PublishJSONStream` / `SubscribeJSONStream` for a lossless append log. Producers accept values supported by `encoding/json`; consumers return `json.RawMessage`.

## Fetching raw groups

Fetch retrieves one group by track name and group sequence without keeping a live subscription:

```go
group, err := consumer.FetchGroup("events", 42, &moq.FetchGroupOptions{Priority: 10})
if err != nil {
    log.Fatal(err)
}
for frame, err := range group.Frames(ctx) {
    if err != nil {
        log.Fatal(err)
    }
    fmt.Printf("%s\n", frame)
}
```

A retained group resolves immediately. To serve a group that is not retained, keep a dynamic handler alive on its producer:

```go
dynamic, err := track.Dynamic()
if err != nil {
    log.Fatal(err)
}
request, err := dynamic.RequestedGroup(ctx)
if err != nil {
    log.Fatal(err)
}
producer, err := request.Accept()
if err != nil {
    log.Fatal(err)
}
_ = producer.WriteFrame(moq.Frame{Payload: loadArchivedFrame(request.Sequence()), TimestampUs: request.Sequence() * 20_000})
_ = producer.Finish()
```

Call `request.Abort(code)` when the requested group cannot be produced. Fetch is currently a single-group operation and is supported by the moq-lite 05+ FETCH wire path.

To serve requests in a loop, range over `dynamic.Requests(ctx)` instead, the same shape `BroadcastDynamic` and `OriginDynamic` use:

```go
for request, err := range dynamic.Requests(ctx) {
    if err != nil {
        if moq.IsShutdown(err) {
            break
        }
        log.Fatal(err)
    }

    producer, err := request.Accept()
    if err != nil {
        log.Fatal(err)
    }
    _ = producer.WriteFrame(loadArchivedFrame(request.Sequence()), request.Sequence()*20_000)
    _ = producer.Finish()
}
```

## Raw track timestamps

Raw tracks carry arbitrary byte payloads. `WriteFrame` takes a `Frame`, whose
`TimestampUs` is a caller-supplied presentation timestamp in microseconds, and raw
tracks default to a microsecond timescale. `ReadFrame` returns the timestamped raw frame:

```go
track, _ := broadcast.PublishTrack("events", nil)
consumer, _ := track.Consume(nil)

_ = track.WriteFrame(moq.Frame{Payload: []byte("ready"), TimestampUs: 20_000})

frame, err := consumer.ReadFrame(ctx)
if err != nil {
	log.Fatal(err)
}
fmt.Println(string(frame.Payload), frame.TimestampUs)
```

## Raw datagrams

Raw tracks can send a single best-effort payload without opening a group stream:

```go
sequence, err := track.AppendDatagram(moq.Frame{Payload: []byte("meter update"), TimestampUs: 42_000})
if err != nil {
    return err
}

datagram, err := consumer.RecvDatagram(ctx)
if err != nil {
    return err
}

for datagram, err := range consumer.Datagrams(ctx) {
    if err != nil {
        return err
    }
    fmt.Println(datagram.Sequence, datagram.TimestampUs)
}
```

Datagrams are delivered as `Datagram{Sequence, TimestampUs, Payload}`. Payloads are capped at 1200 bytes. Delivery requires a datagram-capable transport and lite-05 or newer moq-lite; IETF moq-transport, pre-lite-05, WebSocket, and TCP paths do not deliver them, and there is no stream fallback.

## On-demand broadcasts

Use a dynamic origin when consumers should be able to request whole broadcasts that are not announced:

```go
capacity := uint64(256 * 1024 * 1024)
origin := moq.NewOriginProducerWithOptions(moq.OriginOptions{
	CacheCapacityBytes: &capacity,
})

dynamic := origin.Dynamic()
defer dynamic.Cancel()

for request, err := range dynamic.Requests(ctx) {
	if err != nil {
		if moq.IsShutdown(err) {
			break
		}
		log.Fatal(err)
	}

	path, err := request.Path()
	if err != nil {
		log.Fatal(err)
	}
	if path != "events" {
		if err := request.Abort(404); err != nil {
			log.Fatal(err)
		}
		continue
	}

	broadcast, err := moq.NewBroadcastProducer()
	if err != nil {
		log.Fatal(err)
	}
	track, err := broadcast.PublishTrack("status", nil)
	if err != nil {
		log.Fatal(err)
	}
	if err := request.Accept(broadcast); err != nil {
		log.Fatal(err)
	}
	if err := track.WriteFrame(moq.Frame{Payload: []byte("ready")}); err != nil {
		log.Fatal(err)
	}
}
```

The served broadcast is not announced. It only resolves consumers that call `RequestBroadcast(path)`. Each request arrives as a `BroadcastRequest`; call `Accept(broadcast)` to serve it, or `Abort(code)` to fail the requester.

## Local development

The in-tree `go/wrapper/` directory is the source skeleton; CI publishes it to the [moq-dev/moq-go](https://github.com/moq-dev/moq-go) mirror. To exercise it locally:

```bash
just go check
```

This runs `go/scripts/check.sh`, which builds `moq-ffi` for the host arch, regenerates the bindings with `uniffi-bindgen-go`, stages both the ffi and wrapper modules into `dist/` (wiring the wrapper to the freshly-built ffi via a local `replace`), and runs `go vet`/`go build`/`go test`. Requires `cargo`, `go`, and `uniffi-bindgen-go` on the path; see [moq-ffi](/lib/go/moq-ffi) for the install.

The committed `go/wrapper/go.mod` carries a `require github.com/moq-dev/moq-go-ffi v0.0.0` placeholder; the local `replace` and CI's release-time rewrite supply the real version. Don't "fix" it by hand.

## See also

- Source: [go/wrapper](https://github.com/moq-dev/moq/tree/main/go/wrapper)
- Mirror repo: [moq-dev/moq-go](https://github.com/moq-dev/moq-go)
- Raw bindings it builds on: [moq-ffi](/lib/go/moq-ffi)
- The Rust crates this wraps: [moq-net](/lib/rs/crate/moq-net) + [moq-mux](/lib/rs/crate/moq-mux)
