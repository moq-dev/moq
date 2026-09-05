# [M] Go bindings take context.Context natively

## Goal

`uniffi-bindgen-go` emits `context.Context` as the first parameter of every
generated blocking call, so `go/wrapper` stops carrying a hand-rolled
cancellation token and the generated layer cancels the native task directly.

## Plan

Today every affected `moq-ffi` method takes a trailing
`Option<Arc<MoqCancel>>` purely so the Go wrapper has something to cancel:
`uniffi-bindgen-go` renders a Rust `async fn` as a blocking Go call with no
cancellation handle at all. The token works and costs the other bindings
nothing (it is additive, with a UniFFI argument default), but it exists only
to work around the generator.

A fork of `kixelated/uniffi-bindgen-go` adding native `context.Context`
support already exists locally, on branch `codex/context-cancel-3188`.
Landing it means publishing that branch and pinning the new rev in
`flake.nix`, after which the token becomes redundant for Go and can be removed
from the `moq-ffi` surface.

Decide first whether the fork is worth carrying. Keeping the token is a
supported outcome: it is already shipped and costs nothing outside Go, and a
pinned generator fork is a maintenance obligation on every UniFFI bump. Nobody
should start this until that call is made.

## Required

- The `context.Context` support in `kixelated/uniffi-bindgen-go` is published and pinned in `flake.nix`

## Related

- [Go smoke client](/quest/m0/smoke-go-client.md) - what would actually verify a change to the Go bindings
