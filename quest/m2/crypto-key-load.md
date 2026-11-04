# [S] @moq/crypto loads a stored signing key

## Goal

`@moq/crypto`'s `SecretKey` loads a stored Ed25519 key (raw bytes, and a
non-extractable WebCrypto `CryptoKey` for browsers) and exposes `sign`
publicly, mirroring Rust's `moq_crypto::sign::SecretKey`
(`load_or_generate`, `sign`), so one device key both proves mesh identity
and signs broadcasts.

## Plan

Planned 2026-11-03 (maintainer) from Kixel's `@kixel/mesh`
([moq-dev/kixel.ai#16](https://github.com/moq-dev/kixel.ai/pull/16)), which
had to carry its own Ed25519 and z-base-32 code because `SecretKey` cannot
load a stored key and its `sign` is internal. Keep generated keys working,
keep the browser path non-extractable, and test that a key loaded from bytes
signs what Rust verifies. Ranked at the top of m2 beside the mesh libraries.
