# moq-ffi — Generated Go bindings for moq-ffi

Raw UniFFI-generated bindings for the [`moq-ffi`](../../rs/moq-ffi/) Rust
crate, produced by [`uniffi-bindgen-go`](https://github.com/NordSecurity/uniffi-bindgen-go).

Most users want the ergonomic wrapper at
[`/go/moq-lite/`](../moq-lite/), not this package.

## Regenerate

When the moq-ffi Rust API changes, regenerate the bindings:

```bash
just go gen
```

This runs `uniffi-bindgen-go` against the host's `libmoq_ffi.so`/`.dylib`
and writes `moq/moq.go` and `moq/moq.h`. Both files are committed so users
don't need `uniffi-bindgen-go` installed to build.

## Linking

`cgo.go` links the staticlib at `../../../target/release/libmoq_ffi.a`
(relative to this file). Override via `CGO_LDFLAGS` if your cargo target
dir is elsewhere.
