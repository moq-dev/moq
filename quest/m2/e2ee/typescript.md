# [L] TypeScript E2EE core

## Goal

A reusable TypeScript E2EE layer protects MoQ groups, datagrams, catalogs, and opaque track identities with the shared profile.

It runs in browser and server JavaScript without putting keys or crypto policy in `@moq/net`.

## Plan

- Add a package above `@moq/net` that owns the credential, name derivation, track/domain keys, group frame ordinals, datagram sequences, duplicate window, and typed authentication outcomes. Its public API accepts raw secret bytes or a suitable nonextractable WebCrypto key without serializing secrets.
- Use WebCrypto AES-GCM and HKDF in a bounded ordered asynchronous pump. Propagate backpressure, cancellation, encoder failure, and authentication failure explicitly; never reorder media, grow an unbounded promise queue, or fall back to plaintext.
- Measure 20 ms Opus overhead, browser encryption latency, and queue depth to choose and document the bounded pump defaults without changing the wire profile.
- Wrap whole group and datagram lifecycles so the transform always has the canonical physical track and final transport identity. Do not add a context-free payload callback to `@moq/net`.
- Expose deterministic opaque-name and encrypted-catalog primitives that publish every application catalog representation under protected names. Suppress semantic names outside ciphertext.
- Pass every shared positive and negative vector, then cover asynchronous ordering, queue saturation, protected payload ceilings, group replacement, retransmission, bad-group termination, and bad-datagram events. Exercise grouped tracks on both transports and datagrams on moq-lite.

## Required

- [Encryption profile](/quest/m2/e2ee/profile.md) - fixes the interoperable wire and security contract
