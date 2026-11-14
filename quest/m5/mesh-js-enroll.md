# [S] JS mesh enrollment client

## Goal

`@moq/auth` exports a JS mesh enrollment client matching
`moq_auth::mesh::Client` (`rs/moq-auth/src/mesh/client.rs`): challenge,
enroll with an enrollment key, and refresh by proving the endpoint key,
interoperating with `moq auth serve --mesh-url` and any coordinator that
implements the same contract.

## Plan

Planned 2026-11-03 (maintainer) from a downstream application's JS mesh
client, which implemented this contract then. Port its hardening where it is
generic: https except on loopback, no redirects, bounded time and reply
size, and every reply field checked. Test against `moq auth serve` the way
`rs/moq-auth/src/mesh/contract_tests.rs` does.

Moved to m5 2026-11-12 (maintainer, /quest-audit): that client has since
moved to a certificate enrollment reply, which `moq_auth::mesh::Client`'s
token-only `Enrollment` (`rs/moq-auth/src/mesh/mod.rs`) does not carry, so it
no longer wraps this contract and nothing else asks for a JS client.

## Required

- [`@moq/crypto` loads a stored signing key](/quest/m4/crypto-key-load.md) - the endpoint key this proves
