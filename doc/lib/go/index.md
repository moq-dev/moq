---
title: Go Libraries
description: Go module for Media over QUIC
---

# Go Libraries

The Go bindings expose [Media over QUIC](/) to Go applications via cgo. Built on the same Rust core ([moq-ffi](https://crates.io/crates/moq-ffi)) as the Python, Kotlin, and Swift packages, generated with [uniffi-bindgen-go](https://github.com/NordSecurity/uniffi-bindgen-go).

## Packages

### moq

A single Go module that exposes the UniFFI surface as ordinary Go types. The module ships prebuilt `libmoq_ffi.a` per supported platform and links statically through cgo, so consumers don't need a Rust toolchain or a runtime shared library on their path.

**Supported platforms:**

- `linux/amd64`, `linux/arm64`
- `darwin/amd64`, `darwin/arm64`
- `windows/amd64`

[Learn more](/lib/go/moq)

## Installation

The module lives in [moq-dev/moq-go](https://github.com/moq-dev/moq-go), a mirror repo populated by CI on every `moq-ffi-v*` tag.

```bash
go get github.com/moq-dev/moq-go@v0.2.11
```

```go
import "github.com/moq-dev/moq-go/moq"
```

cgo picks the right `libmoq_ffi.a` automatically via build tags; no `LD_LIBRARY_PATH` or extra setup required. Building requires `CGO_ENABLED=1` (the default on Unix).

## Source and issues

- Source: [go/](https://github.com/moq-dev/moq/tree/main/go) (in the monorepo)
- Mirror (what `go get` resolves): [moq-dev/moq-go](https://github.com/moq-dev/moq-go)
- README: [go/README.md](https://github.com/moq-dev/moq/blob/main/go/README.md)
