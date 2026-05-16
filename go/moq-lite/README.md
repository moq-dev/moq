# moq-lite — Go bindings for MoQ (Media over QUIC)

An ergonomic Go wrapper around the `moq-ffi` UniFFI bindings. Mirrors the
Python wrapper at [`/py/moq-lite/`](../../py/moq-lite/): same surface,
adapted to idiomatic Go (range-over-func iterators, `context.Context`
cancellation, plain `error` returns).

## Status

Early. The Rust core is mature; these Go bindings are new. The API may
shift before a stable tag.

## Requirements

- Go 1.23+
- A Rust toolchain (the `moq-ffi` staticlib is built from source).

This is intentional: we don't ship pre-built binaries today. If you can't
have Rust on your build machine, file an issue and we'll revisit.

## Build

From the monorepo root:

```bash
just go build    # builds moq-ffi staticlib, then `go build` the Go modules
just go test     # builds + runs `go test`
```

Or directly:

```bash
cargo build --release -p moq-ffi
cd go/moq-lite && go test ./...
```

## Quick start

### Publish

```go
package main

import (
    "context"
    "log"
    "time"

    moq "github.com/moq-dev/moq/go/moq-lite"
)

func main() {
    ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
    defer cancel()

    broadcast, err := moq.NewBroadcastProducer()
    if err != nil { log.Fatal(err) }
    defer broadcast.Close()

    track, err := broadcast.PublishTrack("status")
    if err != nil { log.Fatal(err) }
    defer track.Close()

    client, err := moq.Connect(ctx, "https://relay.example.com", nil)
    if err != nil { log.Fatal(err) }
    defer client.Close()

    if err := client.Publish("robot/arm", broadcast); err != nil { log.Fatal(err) }
    track.WriteFrame([]byte(`{"cmd":"ready"}`))
}
```

### Subscribe

```go
client, err := moq.Connect(ctx, "https://relay.example.com", nil)
if err != nil { log.Fatal(err) }
defer client.Close()

ann, err := client.AnnouncedBroadcast("robot/arm")
if err != nil { log.Fatal(err) }
defer ann.Close()

bc, err := ann.Available(ctx)
if err != nil { log.Fatal(err) }
defer bc.Close()

track, err := bc.SubscribeTrack("status")
if err != nil { log.Fatal(err) }
defer track.Close()

for group, err := range track.Groups(ctx) {
    if err != nil { log.Fatal(err) }
    for frame, err := range group.Frames(ctx) {
        if err != nil { log.Fatal(err) }
        log.Printf("%s", frame)
    }
    group.Close()
}
```

See [`examples/clock/`](./examples/clock/) for a runnable publisher and
subscriber.

## Layout

- `client.go` — `Client` / `Connect()` with automatic origin wiring.
- `origin.go` — `OriginProducer`, `OriginConsumer`, `Announced`,
  `Announcement`, `AnnouncedBroadcast`.
- `publish.go` — `BroadcastProducer`, `TrackProducer`, `GroupProducer`,
  `MediaProducer`.
- `subscribe.go` — `BroadcastConsumer`, `TrackConsumer`, `GroupConsumer`,
  `MediaConsumer`, `CatalogConsumer`.
- `types.go` — re-exports of `Frame`, `Catalog`, `Video`, `Audio`,
  `Dimensions`, `Container` from `moq-ffi`.
- `internal.go` — context bridging helpers.

The raw generated bindings live in
[`../moq-ffi/`](../moq-ffi/) and are normally not used directly.
