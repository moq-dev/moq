# [S] rustls-graviola's QUIC keys

## Goal

A rustls-graviola release whose provider carries QUIC keys, and moq-quic's
graviola adapter (graviola packet keys, RustCrypto header-protection masks) is
deleted along with its `aes` and `chacha20` dependencies. Check: the newest
rustls-graviola on crates.io implements rustls's QUIC traits
([ctz/graviola#156](https://github.com/ctz/graviola/pull/156) merged and
released).

## Plan

Upstream status on 2026-10-25: rustls-graviola 0.4.0 ships no QUIC keys.
ctz/graviola#156, a draft by an iroh developer that ports ctz's `jbp-quic`
branch to noq, has been idle since 2026-03-13. Its remaining TODOs are its
dev-dependencies, ChaCha20 for QUIC, and duplicated examples.
[ctz/graviola#226](https://github.com/ctz/graviola/issues/226) asks for its
merge timeline, unanswered. The quinn blocker ctz named in
[ctz/graviola#98](https://github.com/ctz/graviola/issues/98), using rustls
without ring or aws-lc-rs, no longer applies to noq or moq-quic.

moq can advance it: moq-quic's adapter already passes RFC 9001's A.2 and A.5
vectors and moq-quic's suite on graviola, including ChaCha20 header
protection. Offer on #156 to finish its TODOs or to test it against moq-quic,
working with its author and ctz rather than opening a competing PR.

Decided by the maintainer on 2026-10-25: posting upstream for this quest is
approved, so whoever starts it may post without asking.

If the release lands before [moq-relay on graviola](/quest/m1/pure-crypto/relay-graviola.md),
that quest uses it and never adds the adapter; this one then only advances
upstream.

Public API: none. Wire: none.
