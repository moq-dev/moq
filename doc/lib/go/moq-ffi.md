---
title: moq-ffi (Go)
description: Raw Go bindings for Media over QUIC
---

# moq-ffi

The raw Go bindings for [Media over QUIC](/), generated from [rs/moq-ffi](https://github.com/moq-dev/moq/tree/main/rs/moq-ffi) via [uniffi-bindgen-go](https://github.com/NordSecurity/uniffi-bindgen-go).

**Most callers want the ergonomic [moq](/lib/go/moq) wrapper instead.** This module is the native foundation it builds on. It exposes the UniFFI surface as-is (`MoqClient`, `MoqSession`, `MoqBroadcastProducer`, etc.) with blocking methods and `Next()`-style iterators, and it ships the prebuilt `libmoq_ffi.a` per platform, linked statically through cgo.

It is released lockstep with the `moq-ffi` crate: every `moq-ffi-v*` tag publishes a matching `v<semver>` to the [moq-dev/moq-go-ffi](https://github.com/moq-dev/moq-go-ffi) mirror.

## Install

```bash
go get github.com/moq-dev/moq-go-ffi@latest
```

```go
import moqffi "github.com/moq-dev/moq-go-ffi/moq"
```

The module bundles prebuilt `libmoq_ffi.a` for `linux/amd64`, `linux/arm64`, `darwin/amd64`, `darwin/arm64`, and `windows/amd64`. cgo selects the right archive at link time via build tags; `CGO_ENABLED=1` is required (the default on Unix).

## Local development

The in-tree `go/ffi/` directory is the source skeleton; the generated `moq.go`/`moq.h` and per-platform `.a` files are added at release time by CI, not committed. Run `just go check` to build the host bindings and exercise the stack (this drives both the ffi and [moq](/lib/go/moq) wrapper modules). Install `uniffi-bindgen-go` once:

```bash
cargo install uniffi-bindgen-go \
    --git https://github.com/NordSecurity/uniffi-bindgen-go \
    --tag v0.7.1+v0.31.0
```

## See also

- Source: [go/ffi](https://github.com/moq-dev/moq/tree/main/go/ffi)
- Mirror repo: [moq-dev/moq-go-ffi](https://github.com/moq-dev/moq-go-ffi)
- Ergonomic wrapper: [moq](/lib/go/moq)
- Shared FFI crate (also powers the Python, Kotlin, and Swift bindings): [moq-ffi](https://crates.io/crates/moq-ffi)
