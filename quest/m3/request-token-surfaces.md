# [M] Request tokens beyond Rust

## Goal

When a consumer needs it, a JS, binding, or CLI application can present a
request token on its outgoing requests and, where the consumer verifies
tokens, accept them. Each surface mirrors the Rust names from #5148. Only the
surfaces a named consumer uses ship.

## Plan

Deferred from #5148 and the [in-band auth](/quest/m1/auth/README.md)
per-request boundary (2026-10-12). The request-token hook is an experimental
Rust interoperability hook. The JS setter and accept-side queue, a
`moq --request-token` flag, and a moq-ffi setter wait for a consumer. Start
by asking that consumer which surface it needs. Do not build all three.

Mirror the Rust API that #5148 ships, its names and its opt-in verification
included, rather than designing a new shape per surface. Through moq-ffi, follow the session AUTH shape that
[Bindings](/quest/m1/auth/bindings.md) settles. Update every wrapper, as the
cross-package table requires. A CLI flag keeps the token out of logs and
`Debug`, like Rust's. moq-lite cannot carry a token. Fail loud there, as
Rust's `Unsupported` does.

Public API: new request-token setters, and possibly an acceptor, on the
chosen surfaces. Wire: none.

## Related

- [#5148](https://github.com/moq-dev/moq/pull/5148) - the Rust hook these surfaces expose
- [Relay request tokens](/quest/m3/auth-request-lease.md) - the relay's half, also waiting on a consumer
