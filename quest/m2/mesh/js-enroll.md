# [S] JS mesh enrollment client

## Goal

`@moq/auth` exports a JS mesh enrollment client matching
`moq_auth::mesh::Client` (`rs/moq-auth/src/mesh/client.rs`): challenge,
enroll with an enrollment key, and refresh by proving the endpoint key,
interoperating with `moq auth serve --mesh-url` and any coordinator that
implements the same contract.

## Plan

Planned 2026-11-03 (maintainer) from Kixel's `@kixel/mesh`
([moq-dev/kixel.ai#16](https://github.com/moq-dev/kixel.ai/pull/16)), which
implements this contract in Kixel today; once this lands, `@kixel/mesh`
becomes a thin wrapper over it. Port `@kixel/mesh`'s hardening where it is
generic: https except on loopback, no redirects, bounded time and reply
size, and every reply field checked. Test against `moq auth serve` the way
`rs/moq-auth/src/mesh/contract_tests.rs` does.

## Required

- [`@moq/crypto` loads a stored signing key](/quest/m2/crypto-key-load.md) - the endpoint key this proves
