# End-to-end encrypted MoQ

## Goal

TypeScript and Rust publishers and subscribers interoperate over encrypted MoQ broadcasts while every relay, cache, and control plane remains unable to recover content.

Every application payload and semantic track name is confidential and authenticated. MoQ still exposes the outer broadcast path (an opaque name and the epoch), opaque physical track names, group and frame structure, timestamps, sizes, and traffic patterns; padding and metadata-flow confidentiality are out of scope.

## Plan

The contract is [draft-lcurley-moq-e2ee](/drafts/draft-lcurley-moq-e2ee.md), profile `moq-e2ee-00`, with `drafts/moq-e2ee-00.json` as the shared primitive vectors. It starts from IETF Secure Objects where the object model maps exactly, specifies the moq-lite and datagram bindings that draft does not cover, and records intentional differences from SFrame and the experimental `moq-secure` format linked from [#3023](https://github.com/moq-dev/moq/issues/3023).

### Security boundary

- The CDN, relay, cache, recorder, and any platform control plane are actively untrusted for content. Authorized publisher and subscriber endpoints are trusted; sender authenticity against another endpoint holding the broadcast secret is not a goal.
- Applications distribute `(context, kid, secret)` credentials over their own authenticated channel. MoQ announcements, catalogs, paths, relay authorization, and platform APIs never distribute or authenticate content keys.
- E2EE is an explicit per-broadcast mode with no plaintext fallback. Authentication failure ends a grouped track with a typed error; a bad datagram is dropped with a typed event.
- Datagram receivers keep a bounded sliding window as operational duplicate suppression. Grouped frames need none: the transport delivers each frame of a group once at its index. The AEAD identity and epoch rules are the security boundary; relays may still delay, reorder, suppress, or replay ciphertext outside that window.

### Epoch and identity

- Every publisher instance mints an epoch, a UUIDv7 in lowercase text, and publishes at `<opaque>/@<epoch>`, where `<opaque>` derives from the credential and the semantic broadcast name without the epoch. The epoch is an input to every HKDF derivation, so a restart, takeover, or explicit group sequence cannot repeat a nonce under a key: nothing is persisted across instances and no generation counter is redistributed. The epoch is the shared [`Epoch`](/quest/m1/epoch.md) primitive. Subscribers discover instances under the `<opaque>/` prefix and take the greatest epoch, which sorts newest; a known full path carries its epoch in the last segment.
- The epoch is untrusted and unauthenticated. A wrong epoch fails authentication and a withheld one denies service; neither can make a nonce repeat, because only the publisher instance chooses what it encrypts under. This is the same trust a cache needs to serve the right instance. Plaintext broadcasts adopt the same epoch by default through [broadcast epochs](/quest/m1/broadcast-epoch/README.md).
- Nothing in the path says the bytes are encrypted. The format after decryption (`meeting.hang`) is inside the opaque name; a plaintext player, exporter, or matcher that opens a protected broadcast finds no catalog it can read and fails with its usual typed refusal, the same as for any format it does not support.
- One 32-byte secret authorizes the whole broadcast; HKDF-SHA-256 derives separate AES-128-GCM keys for each physical track and for grouped-frame versus datagram domains. A grouped frame uses the 96-bit nonce `uint64_be(group) || uint32_be(frame)`; a datagram uses its sequence with frame zero under the datagram domain. Empty AAD: every immutable end-to-end field is in the HKDF info or the nonce.
- Within an instance, group and datagram sequences are allocated monotonically and frames are numbered in write order, which is the whole reuse rule. There is no ciphertext retention or retransmission API: relays forward bytes unchanged, and an application that needs to resend encrypts nothing twice because it never gets the same identity twice.
- Per-key limits are `2^24` AEAD invocations (failed opens included) and `2^36` plaintext bytes. Datagram plaintext is capped at `1200 - 24 - 16` bytes against the widest moq-lite header; a publisher cannot see the Subscribe ID each hop encodes, so the library does not pretend to budget it.

### Library shape

The Rust and TypeScript cores expose the same surface, and nothing else:

- `Credential { context, kid, secret }` accepts the application-owned secret. The application generates and distributes it over its authenticated channel; the library does not mint a secret it cannot return. `credential.path(semantic)` derives the epoch-free opaque broadcast name. Credential is cheap to clone, never serializes the secret, and redacts it from `Debug`.
- `credential.generation(epoch)` binds a discovered or minted epoch. `Generation` owns `name(semantic)` for opaque track names, `produce(track)` and `consume(track)` for protected `moq-net` tracks, and `Epoch::mint()` returns a fresh UUIDv7. Clones share publisher claims so reopening a track cannot reset its nonce counters.
- `track::Producer` appends groups and datagrams and allocates identities; `track::Consumer` yields groups and datagram events. `group::Producer` and `group::Consumer` wrap the whole group lifecycle so every AEAD call has the canonical physical name and transport identity.
- Errors are the draft's typed codes plus the transport's. Nothing catalog-, hang-, or MSF-shaped lives here: a catalog is a track under a derived name, and compression is the catalog owner's job.
- Stateless `seal`/`open` primitives with caller-chosen identities, HKDF labels and info builders, raw key bytes, and process-global claims are not public. The vectors are tested inside each core.

### Application and platform shape

- Deterministic secret-derived physical names hide catalog, codec, role, quality, timeline, and custom-track semantics. Authorized clients derive the encrypted catalog track name, then learn the remaining opaque names from its decrypted contents. Every catalog representation is encrypted; Rust publishers must not emit a plaintext MSF catalog.
- A platform that forwards and meters protected bytes must never preview, record, archive, transmux, transcode, transcribe, compose, or inspect them, rejecting those paths before opening a processing session or writing product state. Applications needing those operations terminate E2EE outside the platform. A platform classifies protected broadcasts by its own credential or product state, never by name; the moq.pro (downstream) exclusion classifier and dashboard work stay downstream.
- The first proof covers browser TypeScript and native Rust publication and playback in both directions, with grouped audio and video over both moq-lite and MoQ Transport. Shared vectors cover groups and moq-lite datagrams; MoQ Transport has no datagram delivery.

## Quests

- [Receive failure](/quest/m1/e2ee/receiver-failure.md) - a bad grouped frame wakes and terminates every pending read
- [TypeScript E2EE core](/quest/m1/e2ee/typescript.md) - the `@moq/e2ee` package
  mirroring the Rust surface, with WebCrypto in a serial pump
- [Rust protected publisher seams](/quest/m1/e2ee/rust-publish.md) - Rust media
  and catalog publishers accept opaque physical names and emit no plaintext
  semantic catalog in E2EE mode
- [Encrypted browser components](/quest/m1/e2ee/browser.md) - browser publish
  and watch components handle encrypted audio and video without leaking
  credentials into browser-owned surfaces
- [Encrypted native CLI](/quest/m1/e2ee/cli.md) - the native CLI publishes and
  plays encrypted media without exposing credentials through process metadata
  or logs
- [Cross-language encrypted proof](/quest/m1/e2ee/interop.md) - browser
  TypeScript and native Rust exchange encrypted media both ways, with relays
  forwarding and metering blind

## Closes

- [#2277](https://github.com/moq-dev/moq/issues/2277) - close this issue when the quest finishes
- [#3023](https://github.com/moq-dev/moq/issues/3023) - close this issue when the quest finishes

## Related

- [Epoch primitive](/quest/m1/epoch.md) - the shared `Epoch` type the protected path ends in
- [archive](/quest/m1/archive/README.md) - protected broadcasts are deliberately outside recording and replay formats
