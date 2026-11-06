# [M] Request tokens beyond Rust

## Goal

When a consumer needs it, a JS, binding, or CLI application can present a
request token on its outgoing requests and, where the consumer verifies
tokens, accept them. Each surface mirrors the Rust names. Only the surfaces a
named consumer uses ship.

## Plan

✅ Decided 2026-10-15 (maintainer, #5148): a request token is one kind of
auth. Incoming ones already arrive on the session's `requests()` queue, and
outgoing ones come later through `add()` with automatic naming, sent as
`USE_VALUE` on plain moq-transport. There is no separate request-token setter
or acceptor to mirror.

So each surface here exposes that unified shape once Rust has it, and only
for the surface a named consumer uses. Start by asking that consumer which
surface it needs. Do not build all three. Through moq-ffi, follow the session
AUTH shape: `MoqSession.tokens()` and `MoqSession.grants()`. Update every
wrapper, as the cross-package table requires.

Public API: whatever the chosen surface needs to present and accept auths
per request. Wire: none.

## Related

- [#5148](https://github.com/moq-dev/moq/pull/5148) - incoming request tokens in Rust
