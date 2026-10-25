# Pure-Rust crypto for moq-relay

## Goal

moq-relay's release builds compile no C: its crypto moves off aws-lc-rs onto
graviola and RustCrypto, with no hand-rolled primitive, and aws-lc-rs stays
where graviola does not run.

## Plan

Decided with the maintainer on 2026-10-24: adopt all seven recommendations of
the spike in [kixelated/rust2#13](https://github.com/kixelated/rust2/pull/13),
which has the numbers, prototyped on moq's
[`spike/pure-crypto`](https://github.com/moq-dev/moq/tree/spike/pure-crypto)
branch. In short: a clean `cargo check` of moq-relay retires 24% fewer
instructions and a build 14% fewer, since aws-lc-sys's build script leaves the
critical path; handshakes are about a third faster; AES-GCM packets and relay
throughput are even; ChaCha20-Poly1305 packets are 1.7-3x slower.

Split into three quests, so the relay can move while the upstream change waits.
rust2's [no-C check](https://github.com/kixelated/rust2/blob/main/quest/m3/bench-no-c.md)
keeps C from creeping back into the build it benchmarks.

## Required

- [JWT on graviola](/quest/m2/pure-crypto/jwt-graviola.md) - moq-auth signs and verifies tokens through a graviola-based jsonwebtoken provider, with no `rsa` crate
- [moq-relay on graviola](/quest/m2/pure-crypto/relay-graviola.md) - moq-relay's x86_64 and aarch64 release builds use graviola as their rustls provider and compile no C
- [Provider-neutral rustls-acme](/quest/m2/pure-crypto/acme-upstream.md) - rustls-acme takes any rustls crypto provider upstream, and moq drops its patched copy
