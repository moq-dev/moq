# [L] Preserve structured protocol error codes across FFI and C bindings

## Goal

Every binding (Python, Swift, Kotlin, Go, Dart, C) can read the exact session
or stream code a peer sent, tell session scope from stream scope, match a known
kind, and keep an application or unknown code without loss. Transport and
internal failures stay separate from protocol failures.

## Plan

Rust keeps the structure: `SessionError::App(u16)` and `Unknown(u32)`
(`rs/moq-net/src/error.rs:60-108`). The UniFFI boundary throws it away:
`MoqError` is `#[uniffi(flat_error)]` (`rs/moq-ffi/src/error.rs:2-7`), so every
`moq_net::Error` reaches a binding as `Protocol` plus a message. The C facade
does the same through `libmoq::Error::Moq` (`rs/libmoq/src/error.rs:9-21`) and
the thread-local `moq_error()` string (`rs/libmoq/src/api.rs:736`). Callers can
send application codes through abort and cancel but cannot inspect a received
one, so protocol-defined recovery or policy is impossible outside Rust.

- Remove `flat_error` and export a structured error record: scope (session or
  stream), the numeric code, the known kind when recognized, and the message.
  Keep the broad categories matchable.
- `to_code` is deliberately not injective (`error.rs:89-96`: a received 32-63
  decodes as `Unknown`), so the record carries the received code verbatim
  rather than re-deriving it from the kind.
- C gets getters or an output record; parsing `moq_error()` is not the API.
- Cross-language tests cover a known code, an application code, and an unknown
  code.

Breaking on the binding surface, so it lands on dev. Sized [L]: a `moq-ffi`
shape change walks the whole cross-package sync table (`rs/libmoq`, `py`,
`swift`, `kt`, `dart`, `go/wrapper`, and `doc/lib`).

## Closes

- [#3187](https://github.com/moq-dev/moq/issues/3187) - close this issue when the quest finishes
