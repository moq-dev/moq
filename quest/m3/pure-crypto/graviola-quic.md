# [XS] rustls-graviola's QUIC keys

## Goal

A rustls-graviola release whose provider implements rustls's QUIC traits, so
[moq-relay on graviola](/quest/m3/pure-crypto/relay-graviola.md) needs no
adapter of moq's own. Check: the newest rustls-graviola on crates.io carries
QUIC keys ([ctz/graviola#156](https://github.com/ctz/graviola/pull/156) merged
and released). This quest only waits; delete it once that holds.

## Plan

Upstream status on 2026-10-25: rustls-graviola 0.4.0 ships no QUIC keys.
ctz/graviola#156, a draft porting ctz's `jbp-quic` branch to noq, has been idle
since 2026-03-13; [ctz/graviola#226](https://github.com/ctz/graviola/issues/226)
asks for its merge timeline, unanswered.

Decided by the maintainer on 2026-10-26: moq waits passively and contributes no
code upstream, since the relay switch is deferred.

Public API: none. Wire: none.
