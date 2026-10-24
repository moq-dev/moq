# [L] moq-relay on graviola

## Goal

moq-relay's release targets on x86_64 and aarch64 use graviola as their rustls
provider, and their graph compiles no C: no aws-lc-sys, ring, `cc`, or `cmake`,
checked with `cargo tree -i` for each release target triple. Every other target, and every other crate that uses
aws-lc-rs today, keeps it.

## Plan

Decided with the maintainer on 2026-10-24: adopt
[kixelated/rust2#13](https://github.com/kixelated/rust2/pull/13)'s
recommendations, starting from moq's
[`spike/pure-crypto`](https://github.com/moq-dev/moq/tree/spike/pure-crypto)
branch:

- graviola only supports x86_64 and aarch64, so it is the provider for
  moq-relay's release targets there; aws-lc-rs stays the default elsewhere.
- moq-tokio refuses `aws-lc-rs` and `graviola` enabled at once. With both on,
  aws-lc-rs won silently, which hid a graviola-only bug in the spike and would
  make a workspace build benchmark aws-lc. CI builds and tests moq-relay alone,
  so feature unification cannot pick the provider.
- Keep moq-quic's QUIC adapter for graviola (graviola seals packets, RustCrypto
  computes header-protection masks) until rustls-graviola ships QUIC keys
  ([ctz/graviola#156](https://github.com/ctz/graviola/pull/156)), then delete it.
- rustls-acme refuses to build without ring or aws-lc-rs, so moq carries the
  spike's patched copy until [the upstream hook](/quest/m2/pure-crypto/acme-upstream.md)
  lands.
- Accept ChaCha20-Poly1305 on graviola being 1.7-3x slower than aws-lc per
  packet, and record it. It only matters for peers that prefer ChaCha20, which
  lack AES hardware.

Public API: moq-tokio's and moq-quic's crypto features; moq-relay's TLS provider
on its release targets. Wire: none.

## Required

- [JWT on graviola](/quest/m2/pure-crypto/jwt-graviola.md) - moq-relay never ships the `rust_crypto` backend's `rsa` in between

## Related

- [moq-quic's crypto is rustls](/quest/m1/quic-api/crypto.md) - reshapes the crypto surface the QUIC adapter plugs into
