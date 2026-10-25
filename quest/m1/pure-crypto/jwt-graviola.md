# [M] JWT on graviola

## Goal

moq-auth signs and verifies JWTs through a jsonwebtoken `CryptoProvider` built
on graviola, so moq-relay's graph holds neither the `rsa` crate nor the second
RustCrypto generation that jsonwebtoken's `rust_crypto` backend pins, and every
algorithm moq-auth accepts today still verifies.

## Plan

Decided with the maintainer on 2026-10-24, from
[kixelated/rust2#13](https://github.com/kixelated/rust2/pull/13)'s decision 3:
the spike's `rust_crypto` backend signs RS* and PS* with the `rsa` crate, which
is exposed to the Marvin timing attack (RUSTSEC-2023-0071; `deny.toml`'s ignore
of it relies on aws-lc-rs), and adds p256/p384 0.13, ecdsa 0.16, crypto-bigint
0.5, and ed25519-dalek 2 to the graph. graviola has constant-time RSA, ECDSA,
Ed25519, and HMAC.

Never hand-roll a primitive; the provider only adapts graviola to jsonwebtoken's
traits.

jsonwebtoken 11 needs no upstream change: its `CryptoProvider` takes custom
signer and verifier factories. It has only a process-wide default, so moq-auth
installs its provider only when none is installed, as the spike does, and an
embedder's choice wins.

Decided by the maintainer on 2026-10-25: the provider stays private in
moq-auth, since moq is its only consumer.

Public API: none expected; say so if moq-auth's key types change. Wire: none.

## Related

- [moq-relay on graviola](/quest/m1/pure-crypto/relay-graviola.md) - the rest of the relay's crypto
