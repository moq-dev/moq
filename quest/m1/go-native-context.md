# [M] Go bindings take context.Context natively

## Goal

`uniffi-bindgen-go` emits `context.Context` as the first parameter of every
generated blocking call, so `go/wrapper` stops carrying a hand-rolled
cancellation token and the generated layer cancels the native task directly.

## Plan

Today every affected `moq-ffi` method takes a trailing
`Option<Arc<MoqCancel>>` (rs/moq-ffi/src/cancel.rs:18, applied through
`cancel::guard` at :44) purely so the Go wrapper has something to cancel:
`uniffi-bindgen-go` renders a Rust `async fn` as a blocking Go call with no
cancellation handle at all. The token works and costs the other bindings
nothing (it is additive, with a UniFFI argument default), but it exists only
to work around the generator.

The generator is already our fork: `kixelated/uniffi-bindgen-go` carries the
uniffi 0.32 port that upstream lacks (upstream's latest is v0.7.1+v0.31.0;
flake.nix:267-272). The `context.Context` support sits on that fork's
`codex/context-cancel-3188` branch. Landing it means tagging a release from
that branch and bumping the pin in the six files that name the generator
version together: flake.nix:273-281, `.github/workflows/release-go-ffi.yml`
(repo and revision at :30-31), rs/moq-ffi/build.sh:175, go/ffi/README.md:30-31,
go/scripts/check.sh:33, and go/scripts/stage.sh:60. After that the token is
redundant for Go and comes off the `moq-ffi` surface on `dev`.

## Required

- `kixelated/uniffi-bindgen-go` tags a release from `codex/context-cancel-3188`

## Closes

- [#3188](https://github.com/moq-dev/moq/issues/3188) - close this issue when the quest finishes
