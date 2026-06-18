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

## Error handling

A server can reject the connection on auth grounds: `ErrMoqErrorUnauthorized` (HTTP 401) or `ErrMoqErrorForbidden` (HTTP 403). These are terminal: retrying without new credentials won't help, so handle them separately from a transient transport failure. The `moq.IsAuthError` helper catches both:

```go
session, err := client.Connect("https://relay.example.com")
if moq.IsAuthError(err) {
    // Prompt for credentials; don't reconnect.
}
```

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
