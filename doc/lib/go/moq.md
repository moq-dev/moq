---
title: moq (Go)
description: Go module for Media over QUIC
---

# moq

The Go module for [Media over QUIC](/).

A thin wrapper around the UniFFI-generated bindings, exposing the same `MoqClient`, `MoqSession`, `MoqBroadcastProducer`, etc. types as the Python, Kotlin, and Swift packages.

## Install

```bash
go get github.com/moq-dev/moq-go@v0.2.11
```

```go
import "github.com/moq-dev/moq-go/moq"
```

The module bundles prebuilt `libmoq_ffi.a` for `linux/amd64`, `linux/arm64`, `darwin/amd64`, `darwin/arm64`, and `windows/amd64`. cgo selects the right archive at link time via build tags.

## Subscribe

The generated bindings expose pull-based `Next()` handles. The hand-written iterators in `moq/iter.go` wrap those as range-over-func, so streams read idiomatically:

```go
consumer := session.Consumer()
for announcement, err := range consumer.Announcements("demos/") {
    if err != nil {
        log.Fatal(err)
    }

    catalog, err := announcement.Broadcast().SubscribeCatalog()
    if err != nil {
        log.Fatal(err)
    }
    for update, err := range catalog.Updates() {
        if err != nil {
            log.Fatal(err)
        }
        fmt.Println("catalog:", update)
    }
}
```

`Announcements` cancels the subscription it acquired when the loop ends. The consumer-owned iterators (`Groups`, `Frames`, `Updates`) leave the consumer intact, so cancel it yourself to tear down the underlying stream.

## Local development

The in-tree `go/` directory is the source skeleton; it's not a buildable Go module on its own (the generated `moq.go` and per-platform `.a` files are added at release time by CI, not committed). To exercise it locally:

```bash
just go check
```

This runs `go/scripts/check.sh`, which builds `moq-ffi` for the host arch, regenerates the bindings with `uniffi-bindgen-go`, stages everything into the workspace's `dist/` working dir, and runs `go vet`/`go build`/`go test` from the staged copy. Requires `cargo`, `go`, and `uniffi-bindgen-go` on the path. Install the latter once:

```bash
cargo install uniffi-bindgen-go \
    --git https://github.com/NordSecurity/uniffi-bindgen-go \
    --tag v0.7.1+v0.31.0
```

## See also

- Source: [go/moq](https://github.com/moq-dev/moq/tree/main/go/moq)
- Mirror repo: [moq-dev/moq-go](https://github.com/moq-dev/moq-go)
- The Rust crates this wraps: [moq-net](/lib/rs/crate/moq-net) + [moq-mux](/lib/rs/crate/moq-mux)
- Shared FFI layer (also powers the Python, Kotlin, and Swift bindings): [moq-ffi](https://crates.io/crates/moq-ffi)
