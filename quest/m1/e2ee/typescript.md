# [L] TypeScript E2EE core

## Goal

`@moq/e2ee` implements profile `moq-e2ee-00` in browser and server JavaScript
with the library shape in the [questline](/quest/m1/e2ee/README.md), mirroring
the Rust crate name for name, without putting keys or crypto policy in `@moq/net`.

## Plan

- Add the package above `@moq/net`. `Credential` accepts a 32-byte secret or a nonextractable HKDF `CryptoKey` with `deriveBits` usage, rejects extractable keys, and never serializes the secret. `credential.path(semantic)` returns the opaque broadcast name and `credential.generation(epoch)` returns a `Generation` with `name()`, `produce()`, `consume()`, and a `mint()` that returns a lowercase UUIDv7 from `Date.now()` plus `crypto.getRandomValues`.
- Mirror the Rust modules: `Track.Producer`, `Track.Consumer`, `Group.Producer`, `Group.Consumer`, `Datagram.Event`, and a `Failure` whose `code` is the draft's typed set. No catalog helpers, no exported constants beyond the profile limits an application sizes payloads with, no stateless `protect`/`open` with caller-chosen identities, and no test-only methods on public classes.
- WebCrypto AES-GCM is async, so each producer and consumer runs a serial pump: one AEAD call in flight, completions in submit order, a bounded waiter queue that throws when full. Depth one keeps frame ordinals equal to the inner `@moq/net` write order without reservation sets; raise it only when a measured capture burst needs it. Propagate backpressure, cancellation, and authentication failure explicitly; never reorder media or fall back to plaintext.
- Identity state is two counters per key (invocations, plaintext bytes) and one monotonic sequence per track. Check payload size before taking a frame ordinal so a rejected write never burns an identity. Datagram receivers keep a 1024-bit sliding bitmask marked only after a successful open; failed opens count against the key; a grouped authentication failure closes the ordered track.
- Pass every vector in `drafts/moq-e2ee-00.json` against [draft-lcurley-moq-e2ee](/drafts/draft-lcurley-moq-e2ee.md), then cover pump ordering, saturation, and cancellation, protected payload ceilings, monotonic allocation, per-key exhaustion, bounded datagram suppression, bad-group termination, bad-datagram events, and a new epoch authenticating while the old keys do not. Exercise grouped tracks on both transports and datagrams on moq-lite via `insertDatagram`.
- Add the package to `doc/lib/js/index.md` and the workspace; leave it unpublished until the browser components consume it.
