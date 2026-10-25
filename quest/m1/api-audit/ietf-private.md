# [M] moq-transport stays private

## Goal

No public moq-net or `@moq/net` item exists only for moq-transport, except
the recorded exceptions below. A client names its request target once, as a
URL, and each version sends the part it carries.

## Plan

Decided 2026-10-23 (API audit follow-up, maintainer):

- `track::Request::stage` and `track::Staged`, used only by the moq-transport
  subscriber, go private with the other session-only items in
  [session-only items stay private](/quest/m1/api-audit/model-private.md).
- `Client::with_path` and `Client::with_authority` become one
  `Client::with_url(url::Url)`. moq-net gains the `url` dependency and owns
  what moq-tokio's `setup_path` and `setup_authority` decide today
  (`rs/moq-tokio/src/client.rs`): the scheme says what rides SETUP (path for
  moq-lite, path and authority for moq-transport, nothing where the URI
  travels in the transport). moq-tokio, moq-uring, moq-wasm, and moq-ffi call
  it instead.
- `transport::Binding { Uri, InBand }` becomes `transport::Address { Uri, Setup }`
  with `fn address()`: "binding" means a language binding in this repository.

Recorded exceptions, so the next audit skips them:

- moq-transport-only error variants (`Error::{InvalidJoiningRequestId,
  InvalidRange, Remote, UnsupportedExtension}`, the `DecodeError` auth-token
  variants) and the session error codes the lite draft registers with
  moq-transport's meaning stay public. Errors are fine; only dead ones go
  ([net-exports](/quest/m1/api-audit/net-exports.md)).
- `setup::Extensions::{auth, solicit, epoch}` stay as interop knobs, and so
  does the relay's `[listen|connect.extensions]` TOML. Document each field
  as affecting moq-transport only.
- `setup::Token` and `server::Handshake::token` stay: lite-07's SETUP
  Token parameter shares them, so they carry no moq-transport wording.

Public API: moq-net breaking (`with_url`, `Address`), and every transport
crate and binding that sets a path. Wire: none.
