# moq.dev/moq

Ergonomic Go bindings for [Media over QUIC](https://datatracker.ietf.org/doc/draft-lcurley-moq-lite/): real-time pub/sub with built-in caching, fan-out, and prioritization.

This is the package most callers want. It wraps the raw [`moq.dev/moq-ffi`](../ffi) bindings in idiomatic Go: `context.Context` cancellation, Go `error` returns, and Go 1.23 range-over-func iterators (`iter.Seq2`) for streams.

The import path is a vanity path: moq.dev serves a `go-import` meta tag pointing
the go command at the [moq-dev/moq-go](https://github.com/moq-dev/moq-go) mirror,
so the module can move without breaking consumers. The package sits at the module
root, which is why `import "moq.dev/moq"` binds `moq` with no alias.

The module is versioned independently of the native core. CI re-publishes it on
every `moq-ffi-v*` tag with its `require` bumped to the newest `moq.dev/moq-ffi`,
so `go get moq.dev/moq@latest` always pulls the latest native core.

## Install

```bash
go get moq.dev/moq@latest
```

`CGO_ENABLED=1` is required (the default on Unix); the prebuilt `libmoq_ffi.a` comes transitively from `moq.dev/moq-ffi`, so there is no Rust toolchain or shared-library setup.

## Quick start

```go
import (
	"context"
	"fmt"
	"log"
	"moq.dev/moq"
)

ctx := context.Background()

client, err := moq.Dial(ctx, "https://relay.example.com", moq.ClientConfig{})
if err != nil {
	log.Fatal(err)
}
defer client.Close()

announced, err := client.Announced(moq.AnnounceOptions{Prefix: "demos/"})
if err != nil {
	log.Fatal(err)
}
for event, err := range announced.All(ctx) {
	if err != nil {
		if moq.IsShutdown(err) {
			break
		}
		log.Fatal(err)
	}
	switch event := event.(type) {
	case moq.AnnounceEventStart:
		// Prefix stays origin-relative; Captures reports wildcard matches.
		fmt.Println("got broadcast", event.Announce.Prefix)
	case moq.AnnounceEventRestart: // a new publisher run at the same path
		fmt.Println("restarted broadcast", event.Announce.Prefix)
	}
}
```

## Common APIs

`Dial` takes a `ClientConfig` whose names and nesting mirror moq-ffi's
`MoqClientConfig`. Client TLS is configured in `ClientConfig.TLS` with `Roots`,
`SystemRoots`, and `Fingerprints`; set `Cert` and `Key` for mTLS.
Use fingerprints with `Server.CertFingerprints()` when
pinning a generated self-signed certificate.

`Client.Session().Stats()` returns a connection stats snapshot. Fields are nil
when the transport backend does not report that metric yet.

An encoder you manage can claim a share of `Client.Session().Allocator()` with
`Reserve(demand, maxBps)`, using the track's `Demand()`. Keep one reservation and change its ceiling
with `Update(maxBps)`. Call `Update(0)` before abandoning or replacing it to
return its share and automatic probe ceiling immediately, even while the track
stays subscribed. A later update can resume the same claim. While the track
stays open, dropping the Go reference alone leaves release to garbage collection.

Incoming server requests expose the query-free `Path()` before `Accept()`. It
is consistent across transports and returns an empty string for the root or
missing path. `Query()` returns the encoded query and may contain credentials.

Media lives in `moq.dev/moq/media`. `NewAudioTrackProducer` / `NewVideoTrackProducer`
take a broadcast, a `Named` or `Requested` target, and an `AudioInit` / `VideoInit`.
A `Named` target chooses a track name or lets the importer derive one; a `Requested`
target takes over a subscriber's pending request. A duplicate name fails.
`VideoInit.Hint` seeds fields the bitstream cannot supply. `NewContainerProducer` demuxes
a container from chunks split anywhere, such as a byte stream, into its own tracks.
Share `NewTimebase(broadcast)` in its `ContainerInit` for containers on one source
timeline; omitting it gives each an independent offset. Timebases belong to one broadcast and do not keep it open. Call `Close` to
release the timebase handle on the calling goroutine.
`NewCatalogProducer` holds its broadcast weakly and owns catalog properties and sections.
`NewCatalogConsumer`, `NewTrackConsumer`, and `FetchGroup` read
catalog snapshots, live media, and one fetched media group. The last two take a `Rendition`
(`VideoRendition` or `AudioRendition`: a catalog ID and its config) and follow its broadcast
and track themselves. `NewGroupConsumer`
decodes a raw group, such as one from `moq.TrackConsumer.RecvGroup` for arrival-order reads.
Frames and `Flush` use `time.Duration`; `Demand()` owns track names and subscriber waits.

`BroadcastProducer.Requests()` accepts subscriber-requested tracks. Call
`TrackRequest.Accept()` for raw tracks, or pass the request as `media.Requested`
so the media importer selects the timescale.

JSON tracks live in the `moq.dev/moq/json` subpackage, mirroring the `moq-json` crate.
Import it under an alias next to `encoding/json`. Each type wraps a track:
`NewSnapshotProducer` / `NewStreamProducer` take over a `TrackProducer` and advertise it
in the broadcast's catalog, and `NewSnapshotConsumer` / `NewStreamConsumer` take over a
`TrackConsumer` that has not read a group yet. Snapshots carry lossy latest state; streams
carry every record in order. `NewWindowProducer` / `NewWindowConsumer` carry a sliding window
of records a reader can join at any point; the catalog has no window mode, so the producer
takes only the track. Producers accept any `encoding/json` value; consumers return
`json.RawMessage` so callers choose their own decoded type.
`Update` / `Append` / `Push` / `Pop` take a final `*time.Duration` on the broadcast clock; nil publishes untimed.
Raw tracks with a nil timescale are untimed; timed tracks require matching timestamps.
`media.CatalogProducer.Now()` samples and fixes the catalog clock for explicit local-event stamps.
Opaque bytes go on a plain `TrackProducer`.

```go
import moqjson "moq.dev/moq/json"

publication, _ := client.CreateBroadcast("room")
broadcast := publication.Broadcast()
// JSON is small and repetitive, so compress the track on the wire.
track, _ := broadcast.PublishTrack("status", &moq.TrackInfo{Compress: true})
status, _ := moqjson.NewSnapshotProducer(broadcast, track, moqjson.SnapshotOptions{})
_ = status.Update(map[string]any{"viewers": 42}, nil)
_ = publication.Announce(moq.AnnounceRoute{})
```

Snapshot and window producers take `SnapshotOptions` or `WindowOptions`; the stream producer and
every consumer take only the track, and a consumer reads a compressed track like a plain one.

## Errors

All FFI errors come back as the `moq.Error` type. The error variants are
re-exported as sentinels (`moq.ErrClosed`, `moq.ErrUnauthorized`, ...) so you can
`errors.Is` against them without importing `moq.dev/moq-ffi`. Two helpers cover the
common cases: `moq.IsShutdown(err)` (a stream ended because it was cancelled or
the session closed, i.e. not a real failure) and `moq.IsAuthError(err)` (HTTP
401/403 or an expired token).

Every call that can block takes a `context.Context` and aborts cleanly when it is
cancelled, leaving nothing running in the background. A one-shot call (a subscribe,
a fetch, `Request`, `Resolve`, `WantedChanged`, `Used`/`Unused`, `Server.Accept`) aborts on
its own and leaves the object usable; a stream read cancels the stream it reads.
See the package doc for details.

## Raw datagrams

Raw tracks support best-effort datagrams alongside groups: `TrackProducer.AppendDatagram`
sends one `Frame` and returns its sequence number, while `TrackConsumer.RecvDatagram`
and `TrackConsumer.Datagrams` receive them in arrival order. Payloads are capped at
1200 bytes. Datagram delivery requires a datagram-capable transport and lite-05 or
newer moq-lite, or moq-transport; pre-lite-05, WebSocket, and TCP paths do not
deliver them, and there is no stream fallback.

## Versioning

`VERSION` holds the human-owned `MAJOR.MINOR` line (the wrapper API version). Bump it in a PR only for a breaking change to the wrapper's own API. The patch number is derived by CI from the existing mirror tags, so every release (whether triggered by a wrapper change or by a new `moq.dev/moq-ffi`) just takes the next patch on that line.

The committed `go.mod` carries a `require moq.dev/moq-ffi v0.0.0` **placeholder**. Do not "fix" it or add a `replace`: `just go check` injects a local `replace` to the freshly-generated bindings, and CI rewrites the `require` to the latest published `moq.dev/moq-ffi` at release time. Because Go resolves to the maximum version across the build graph, that `require` is a floor. Consumers always get an ffi at least as new as the wrapper was built against.

## Local development

Run `just go check`: it builds `moq-ffi` for the host, regenerates the bindings, stages both modules into `dist/` with a `replace` wiring the wrapper to the local ffi, and runs `go build`/`go vet`/`go test`. See [../ffi/README.md](../ffi/README.md) for the `uniffi-bindgen-go` install.
