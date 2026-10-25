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

Moved to m1 on 2026-10-25 (maintainer), so the cut ships it. The relay moves
first and carries a workaround for each missing upstream piece; one child per
upstream change then deletes its workaround. Those children wait on other
projects, so the cut does not wait for them: any still open when the cut is
ready moves to m2.

rust2's [no-C check](https://github.com/kixelated/rust2/blob/main/quest/m3/bench-no-c.md)
keeps C from creeping back into the build it benchmarks.

The spike's other workarounds need no upstream change: rcgen without a crypto
backend refuses a certificate with no serial number by design, so the caller
picks a random one, and jsonwebtoken 11 already takes a custom
`CryptoProvider`.

## Required

- [JWT on graviola](/quest/m1/pure-crypto/jwt-graviola.md) - moq-auth signs and verifies tokens through a graviola-based jsonwebtoken provider, with no `rsa` crate
- [moq-relay on graviola](/quest/m1/pure-crypto/relay-graviola.md) - moq-relay's x86_64 and aarch64 release builds use graviola as their rustls provider and compile no C
- [Provider-neutral ACME](/quest/m1/pure-crypto/acme-upstream.md) - moq-relay's ACME runs on graviola through a released upstream crate, and moq drops its patched rustls-acme
- [rustls-graviola's QUIC keys](/quest/m1/pure-crypto/graviola-quic.md) - a rustls-graviola release ships QUIC keys, and moq-quic's adapter is deleted
- [The next graviola release](/quest/m1/pure-crypto/graviola-release.md) - moq pins a graviola with ECDSA key generation and faster ChaCha20-Poly1305, drops RustCrypto's `p256`, and re-measures the ChaCha20 gap
