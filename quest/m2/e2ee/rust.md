# [L] Rust E2EE core

## Goal

A reusable Rust E2EE layer protects MoQ groups, datagrams, catalogs, and opaque track identities with the shared profile.

It gives native publishers and subscribers the same context-aware contract as TypeScript without putting content keys in relays.

## Plan

- Add a crate above `moq-net` that owns the credential, name derivation, track/domain keys, group frame ordinals, datagram sequences, duplicate window, and typed authentication outcomes.
- Wrap complete producer and consumer group/datagram lifecycles rather than installing a bytes-only `PayloadProcessor` inside `moq-net`. Use the linked `moq-secure` work from [#3023](https://github.com/moq-dev/moq/issues/3023) as implementation prior art only where it satisfies the settled profile.
- Make exclusive identity ownership and ordered publication explicit in types. Reject sequence reuse and exhaustion before encryption, retain ciphertext for retransmission, zeroize owned key bytes, and keep secrets out of errors, tracing, debug output, and serialization.
- Measure grouped-frame and datagram throughput to choose and document bounded processing defaults without changing the wire profile.
- Pass every shared positive and negative vector, then cover protected payload ceilings, group replacement, retransmission, bad-group termination, bad-datagram events, and cancellation. Exercise grouped tracks on both transports and datagrams on moq-lite.

## Required

- [Encryption profile](/quest/m2/e2ee/profile.md) - fixes the interoperable wire and security contract
