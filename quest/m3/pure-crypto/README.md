# Pure-Rust crypto for moq-relay

## Goal

moq-relay's release builds compile no C: its crypto moves off aws-lc-rs onto
graviola, with no hand-rolled primitive, once upstream graviola and ACME
releases make the switch cheap for moq. aws-lc-rs stays where graviola does not
run.

## Plan

Decided with the maintainer on 2026-10-24 to adopt the spike in
[kixelated/rust2#13](https://github.com/kixelated/rust2/pull/13), which has the
numbers, prototyped on moq's
[`spike/pure-crypto`](https://github.com/moq-dev/moq/tree/spike/pure-crypto)
branch: a clean `cargo build -p moq-relay` retires 14% fewer instructions,
handshakes are about a third faster, AES-GCM packets and relay throughput are
even, and ChaCha20-Poly1305 packets are 1.7-3x slower.

Deferred to m3 by the maintainer on 2026-10-26, reversing the 2026-10-25 move
to m1. Adopting now costs more than it returns:

- Only the relay drops C. moq-rtc, moq-e2ee, moq-cli, moq-ffi, moq-uring, and
  the rest stay on aws-lc-rs, so workspace builds, dev loops, and most CI still
  compile aws-lc-sys, and incremental builds never rebuild it. The release
  targets already build aws-lc-sys with `cargo zigbuild`.
- moq would carry two providers and their exclusion gate, a QUIC adapter mixing
  graviola and RustCrypto, a mirrored CPU feature list, a jsonwebtoken
  provider, and release builds without the default `acme` feature.
- graviola refuses hosts that mask ADX or AVX2, where aws-lc runs today, and
  is young with no published audit.

Each upstream gap is a child condition quest that only checks upstream; moq
does not contribute code to close them. Once all three are released,
[moq-relay on graviola](/quest/m3/pure-crypto/relay-graviola.md) is re-planned
and the moq side shrinks to a provider feature. The draft
[moq-dev/moq#6861](https://github.com/moq-dev/moq/pull/6861), JWT on graviola,
was closed with the deferral.

## Required

- [rustls-graviola's QUIC keys](/quest/m3/pure-crypto/graviola-quic.md) - waiting on a rustls-graviola release with QUIC keys, so moq never carries its own adapter
- [graviola's own CPU check](/quest/m3/pure-crypto/graviola-cpu-check.md) - waiting on ctz/graviola#227, so the relay never mirrors graviola's feature list
- [ACME on graviola](/quest/m3/pure-crypto/acme-upstream.md) - waiting on rustls-acme gaining a provider hook or instant-acme 0.9, so graviola builds keep ACME
- [moq-relay on graviola](/quest/m3/pure-crypto/relay-graviola.md) - moq-relay's x86_64 and aarch64 release builds use graviola as their rustls provider and compile no C
