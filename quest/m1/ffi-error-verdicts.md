# [S] Bindings ask moq-ffi whether an error is auth, shutdown, or retryable

## Goal

moq-ffi exports the verdicts the bindings now rebuild by matching
`MoqError` variants: whether an error is a graceful shutdown, an
authentication failure, and moq-net's `retry()` answer (same, reroute, or
never). Every binding's helper (`is_shutdown` and `is_auth` in Python,
`isAuth` in Kotlin, `IsAuthError` in Go, and their Swift, Dart, and C++
siblings) calls it, so no binding matches error variants and all of them
agree.

## Plan

From the 2026-11-18 enum-return audit (maintainer, /quest-plan). Each binding
classifies errors by hand today (`py/moq-rs/moq/errors.py`,
`kt/.../Errors.kt`, `go/ffi/moq/errors.go`, `go/wrapper/errors.go`), against
AGENTS.md's "never match on errors", and each list can drift from the others
and from Rust. moq-net's `Error::retry()` and js/net's `retry` are the
sanctioned classifier; expose that, and decide auth and shutdown once in
Rust next to it.

Public API: additive in moq-ffi and every binding; the wrapper helpers keep
their signatures. Wire: none.
