# [L] moq-relay on graviola

## Goal

moq-relay's release targets on x86_64 and aarch64 use graviola as their rustls
provider, and their graph compiles no C: no aws-lc-sys, ring, `cc`, or `cmake`,
checked with `cargo tree -i` for each release target triple. Every other
target, and every other crate that uses aws-lc-rs today, keeps it.

## Plan

Waits for its three upstream condition quests, then is re-planned against
whatever upstream shipped (maintainer, 2026-10-26); see the
[epic](/quest/m3/pure-crypto/README.md) for why. With them released, moq
carries no QUIC adapter, no mirrored CPU feature list, and no ACME exclusion.

Still decided, from
[kixelated/rust2#13](https://github.com/kixelated/rust2/pull/13) and the
maintainer on 2026-10-24 and 2026-10-25:

- graviola only supports x86_64 and aarch64, so it is the provider for
  moq-relay's release targets there; aws-lc-rs stays the default elsewhere.
- moq-tokio refuses `aws-lc-rs` and `graviola` enabled at once. With both on,
  aws-lc-rs won silently in the spike, which hid a graviola-only bug. CI builds
  and tests moq-relay alone, so feature unification cannot pick the provider.
- moq-auth's jsonwebtoken provider moves to graviola too, never through
  jsonwebtoken's `rust_crypto` backend, which signs RS* and PS* with the `rsa`
  crate (Marvin, RUSTSEC-2023-0071) and adds a second RustCrypto generation.
  jsonwebtoken 11 takes a custom `CryptoProvider`; the closed draft
  [moq-dev/moq#6861](https://github.com/moq-dev/moq/pull/6861) has a working
  one to start from. moq-auth installs it only when none is installed.
  moq-auth's key generation and JWK loading, on aws-lc-rs since
  [moq-dev/moq#6898](https://github.com/moq-dev/moq/pull/6898), move with it,
  so moq-auth's aws-lc-rs dependency becomes target-gated like the relay's.
- Accept ChaCha20-Poly1305 on graviola being 1.7-3x slower than aws-lc per
  packet (graviola 0.4.1), and re-measure it on the pinned release. It only
  matters for peers that prefer ChaCha20, which lack AES hardware.
- The relay fails at startup with a clear error naming a missing CPU feature,
  not on its first handshake, and the rollout checks the relays' own hosts
  first. Some VPS hypervisors mask ADX
  ([ctz/graviola#222](https://github.com/ctz/graviola/issues/222)).
- Self-signed keys come from graviola's ECDSA key generation
  ([ctz/graviola#203](https://github.com/ctz/graviola/pull/203), unreleased
  after 0.4.1) if a release carries it, else RustCrypto's `p256`, which is
  pure Rust too, so it is no gate.

Open at re-plan: moq-relay's `io-uring` feature serves QUIC through moq-uring,
which is aws-lc-only.

Public API: moq-tokio's and moq-quic's crypto features; moq-relay's TLS
provider on its release targets. Wire: none.

## Required

- [rustls-graviola's QUIC keys](/quest/m3/pure-crypto/graviola-quic.md) - moq plugs graviola's own QUIC keys into its sealed crypto surface
- [graviola's own CPU check](/quest/m3/pure-crypto/graviola-cpu-check.md) - the startup check calls graviola instead of mirroring it
- [ACME on graviola](/quest/m3/pure-crypto/acme-upstream.md) - graviola builds keep the default `acme` feature
- [moq-quic's crypto is rustls](/quest/m1/quic-api/crypto.md) - the sealed crypto surface graviola plugs into
