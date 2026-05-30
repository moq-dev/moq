---
title: Go Libraries
description: Go module for Media over QUIC
---

# Go Libraries

The Go bindings expose [Media over QUIC](/) to Go applications via cgo. Built on the same Rust core ([moq-ffi](https://crates.io/crates/moq-ffi)) as the Python, Kotlin, and Swift packages, generated with [uniffi-bindgen-go](https://github.com/NordSecurity/uniffi-bindgen-go).

## Packages

### moq

The ergonomic wrapper, and the package most callers want. Idiomatic Go over the raw bindings: `context.Context` cancellation, Go `error` returns, and Go 1.23 `iter.Seq2` iterators for live streams. The wrapper itself is hand-written Go; the native libraries come transitively from `moq-ffi`, so a build still needs `CGO_ENABLED=1`.

[Learn more](/lib/go/moq)

### moq-ffi

The raw UniFFI bindings (`MoqClient`, `MoqSession`, etc.) with blocking methods, plus the prebuilt `libmoq_ffi.a` per platform, linked statically through cgo. Released lockstep with the `moq-ffi` crate. Most callers use `moq` instead.

**Supported platforms:**

- `linux/amd64`, `linux/arm64`
- `darwin/amd64`, `darwin/arm64`
- `windows/amd64`

[Learn more](/lib/go/moq-ffi)

## Installation

```bash
go get github.com/moq-dev/moq-go@latest
```

```go
import "github.com/moq-dev/moq-go/moq"
```

cgo picks the right `libmoq_ffi.a` automatically via build tags; no `LD_LIBRARY_PATH` or extra setup required. Building requires `CGO_ENABLED=1` (the default on Unix). `@latest` always pulls the newest native core: CI re-publishes the wrapper with its `moq-ffi` require bumped on every release.

## Source and issues

- Source: [go/](https://github.com/moq-dev/moq/tree/main/go) (in the monorepo)
- Wrapper mirror (what `go get` resolves): [moq-dev/moq-go](https://github.com/moq-dev/moq-go)
- Raw bindings mirror: [moq-dev/moq-go-ffi](https://github.com/moq-dev/moq-go-ffi)
