# [S] The next graviola release

## Goal

moq pins a graviola release newer than 0.4.1 that carries ECDSA key generation
and the faster ChaCha20-Poly1305. moq-tokio generates self-signed keys with
graviola instead of RustCrypto's `p256`, and the ChaCha20-Poly1305 gap recorded in
[moq-relay on graviola](/quest/m1/pure-crypto/relay-graviola.md) is
re-measured. Check: the newest graviola on crates.io is past 0.4.1.

## Plan

Upstream status on 2026-10-25: graviola 0.4.1 (2026-06-24) is the latest
release. Merged since, unreleased:
[ctz/graviola#203](https://github.com/ctz/graviola/pull/203), ECDSA key
generation;
[ctz/graviola#221](https://github.com/ctz/graviola/pull/221), 64-bit-limb
Poly1305; and [ctz/graviola#212](https://github.com/ctz/graviola/pull/212),
a NEON ChaCha20 for aarch64. Open:
[ctz/graviola#218](https://github.com/ctz/graviola/pull/218), stitched
ChaCha20-Poly1305, and [ctz/graviola#231](https://github.com/ctz/graviola/pull/231),
a faster aarch64 ChaCha20.

ChaCha20 work is already active upstream, so moq contributes measurements, not
code: moq-quic's crypto bench, per packet, on x86_64 and aarch64. Post them
upstream only if they show something graviola's own benchmarks do not. If the
release is slow to come, asking for one is fine.

Decided by the maintainer on 2026-10-25: posting upstream for this quest is
approved, so whoever starts it may post without asking.

If the release lands before moq-relay on graviola, that quest pins it directly,
and this one only re-measures.

Public API: none. Wire: none.
