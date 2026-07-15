# moq-go

Ergonomic Go bindings for [Media over QUIC](https://datatracker.ietf.org/doc/draft-lcurley-moq-lite/): real-time pub/sub with built-in caching, fan-out, and prioritization.

This is the package most callers want. It wraps the raw [`github.com/moq-dev/moq-go-ffi`](../ffi) bindings in idiomatic Go: `context.Context` cancellation, Go `error` returns, and Go 1.23 range-over-func iterators (`iter.Seq2`) for streams.

The published module lives at [moq-dev/moq-go](https://github.com/moq-dev/moq-go), versioned independently of the native core. CI re-publishes it on every `moq-ffi-v*` tag with its `require` bumped to the newest `moq-go-ffi`, so `go get github.com/moq-dev/moq-go@latest` always pulls the latest native core.

## Install

```bash
go get github.com/moq-dev/moq-go@latest
```

```go
import "github.com/moq-dev/moq-go/moq"
```

`CGO_ENABLED=1` is required (the default on Unix); the prebuilt `libmoq_ffi.a` comes transitively from `moq-go-ffi`, so there is no Rust toolchain or shared-library setup.

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
}
```

## Common APIs

Client TLS can be configured with `WithTLSRoots`, `WithTLSSystemRoots`, and
`WithTLSFingerprints`. Use `WithClientTLSCert` and `WithClientTLSKey` for mTLS.
Use fingerprints with `Server.CertFingerprints()` when
pinning a generated self-signed certificate.

`Client.Session().Stats()` returns a connection stats snapshot. Fields are nil
when the transport backend does not report that metric yet.

`BroadcastProducer.Dynamic()` accepts subscriber-requested tracks. Call
`TrackRequest.Accept()` for raw tracks, or `BroadcastProducer.PublishMediaOnTrack()`
for media tracks whose timescale should be selected by the importer.

`PublishMedia`, `PublishMediaOnTrack`, and `PublishMediaStream` accept
`WithVideoHint(moq.VideoHint{...})` for video catalog fields that are known
before the stream reveals them.

JSON tracks are available in two modes. `PublishJSONSnapshot` / `SubscribeJSONSnapshot`
carry lossy latest state, while `PublishJSONStream` / `SubscribeJSONStream` carry every
record in order. Producers accept any `encoding/json` value; consumers return
`json.RawMessage` so callers choose their own decoded type.

## Errors

All FFI errors come back as the `moq.Error` type. The error variants are
re-exported as sentinels (`moq.ErrClosed`, `moq.ErrUnauthorized`, ...) so you can
`errors.Is` against them without importing `moq-go-ffi`. Two helpers cover the
common cases: `moq.IsShutdown(err)` (a stream ended because it was cancelled or
the session closed, i.e. not a real failure) and `moq.IsAuthError(err)` (HTTP
401/403).

Blocking calls take a `context.Context`. Most abort cleanly when the context is
cancelled; the few that have no native cancel (`Used`/`Unused` and
`Server.Accept`) return `ctx.Err()` promptly but keep running in the background
until the owning producer/server is closed. See the package doc for details.

## Raw datagrams

Raw tracks support best-effort datagrams alongside groups: `TrackProducer.AppendDatagram`
sends one payload and returns its sequence number, while `TrackConsumer.RecvDatagram`
and `TrackConsumer.Datagrams` receive them in arrival order. Payloads are capped at
1200 bytes. Datagram delivery requires a datagram-capable transport and lite-05 or
newer moq-lite; IETF moq-transport, pre-lite-05, WebSocket, and TCP paths do not
deliver them, and there is no stream fallback.

## Versioning

`VERSION` holds the human-owned `MAJOR.MINOR` line (the wrapper API version). Bump it in a PR when the wrapper's own API changes. The patch number is derived by CI from the existing mirror tags, so every release (whether triggered by a wrapper change or by a new `moq-go-ffi`) just takes the next patch on that line.

The committed `go.mod` carries a `require github.com/moq-dev/moq-go-ffi v0.0.0` **placeholder**. Do not "fix" it or add a `replace`: `just go check` injects a local `replace` to the freshly-generated bindings, and CI rewrites the `require` to the latest published `moq-go-ffi` at release time. Because Go resolves to the maximum version across the build graph, that `require` is a floor. Consumers always get an ffi at least as new as the wrapper was built against.

## Local development

Run `just go check`: it builds `moq-ffi` for the host, regenerates the bindings, stages both modules into `dist/` with a `replace` wiring the wrapper to the local ffi, and runs `go build`/`go vet`/`go test`. See [../ffi/README.md](../ffi/README.md) for the `uniffi-bindgen-go` install.
