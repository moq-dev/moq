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
  computes header-protection masks) until
  [rustls-graviola ships QUIC keys](/quest/m1/pure-crypto/graviola-quic.md).
- rustls-acme refuses to build without ring or aws-lc-rs, so moq carries the
  spike's patched copy until [provider-neutral ACME](/quest/m1/pure-crypto/acme-upstream.md)
  lands.
- Accept ChaCha20-Poly1305 on graviola being 1.7-3x slower than aws-lc per
  packet, and record it. It only matters for peers that prefer ChaCha20, which
  lack AES hardware. [The next graviola release](/quest/m1/pure-crypto/graviola-release.md)
  re-measures it.

Builds on [moq-quic's crypto reshape](/quest/m1/quic-api/crypto.md), which
seals moq-quic's crypto to rustls and deletes the `aws-lc-rs`/`ring` gates in
`quic::web`; landing graviola first would add a third provider to every gate
that reshape deletes.

graviola panics on its first operation on an x86_64 CPU without AES-NI,
PCLMULQDQ, BMI1, ADX, or AVX2, and some VPS hypervisors mask ADX
([ctz/graviola#222](https://github.com/ctz/graviola/issues/222), on an OVH VPS;
graviola's maintainer treats it as a hypervisor configuration). The relay must
fail at startup on such a CPU, not on its first handshake, and the rollout
checks the relays' own hosts first. Recommendation: touch graviola during
startup so its own check fires, rather than copying its feature list; a public
check ([ctz/graviola#227](https://github.com/ctz/graviola/issues/227)) would
only turn that panic into an error. The alternative, a second x86_64 release
build on aws-lc-rs, doubles that target's artifacts for hosts we have not seen.

Public API: moq-tokio's and moq-quic's crypto features; moq-relay's TLS provider
on its release targets. Wire: none.

## Required

- [JWT on graviola](/quest/m1/pure-crypto/jwt-graviola.md) - moq-relay never ships the `rust_crypto` backend's `rsa` in between
- [moq-quic's crypto is rustls](/quest/m1/quic-api/crypto.md) - the sealed crypto surface the QUIC adapter plugs into
