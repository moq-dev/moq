# End-to-end encrypted MoQ

## Goal

TypeScript and Rust publishers and subscribers interoperate over encrypted MoQ broadcasts while every relay, cache, and control plane remains unable to recover content.

Every application payload and semantic track name is confidential and authenticated. MoQ still exposes the outer broadcast path (an opaque name), the route's epoch, opaque physical track names, group and frame structure, timestamps, sizes, and traffic patterns; padding and metadata-flow confidentiality are out of scope.

## Plan

The contract is [draft-lcurley-moq-e2ee](/drafts/draft-lcurley-moq-e2ee.md), profile `moq-e2ee-00`, with `drafts/moq-e2ee-00.json` as the shared primitive vectors. It starts from IETF Secure Objects where the object model maps exactly, specifies the moq-lite and datagram bindings that draft does not cover, and records intentional differences from SFrame and the experimental `moq-secure` format linked from [#3023](https://github.com/moq-dev/moq/issues/3023).

### Security boundary

- The CDN, relay, cache, recorder, and any platform control plane are actively untrusted for content. Authorized publisher and subscriber endpoints are trusted; sender authenticity against another endpoint holding the broadcast secret is not a goal.
- Applications distribute `(context, kid, secret)` credentials over their own authenticated channel. MoQ announcements, catalogs, paths, relay authorization, and platform APIs never distribute or authenticate content keys.
- E2EE is an explicit per-broadcast mode with no plaintext fallback. Authentication failure ends a grouped track with a typed error; a bad datagram is dropped with a typed event.
- Datagram receivers keep a bounded sliding window as operational duplicate suppression. Grouped frames need none: the transport delivers each frame of a group once at its index. The AEAD identity and epoch rules are the security boundary; relays may still delay, reorder, suppress, or replay ciphertext outside that window.

### Epoch and identity

- Every publisher instance mints an epoch, a UUIDv7 in lowercase text, and publishes at `<opaque>` with the epoch on its route, where `<opaque>` derives from the credential and the semantic broadcast name without the epoch. The epoch is an input to every HKDF derivation, so a restart, takeover, or explicit group sequence cannot repeat a nonce under a key: nothing is persisted across instances and no generation counter is redistributed. The epoch is the shared [`Epoch`](/doc/concept/moq-lite.md#publisher-epochs) primitive. Subscribers resolve `<opaque>` and take the epoch from its route, where the newest wins; on a wire without route epochs the application supplies it.
- An encrypting publisher refuses a shared or explicit epoch (maintainer, 2026-10-08). Two producers handed one epoch would derive the same keys and repeat every nonce. Both cores produce only under an epoch they minted themselves; consuming still binds a discovered one. Rejected: a per-instance salt. State the rule in the draft's epoch section.
- The epoch is untrusted and unauthenticated. A wrong epoch fails authentication and a withheld one denies service; neither can make a nonce repeat, because only the publisher instance chooses what it encrypts under. This is the same trust a cache needs to serve the right instance. First-party plaintext publishers already mint the same route epoch.
- Nothing in the path says the bytes are encrypted. The format after decryption (`meeting.hang`) is inside the opaque name; a plaintext player, exporter, or matcher that opens a protected broadcast finds no catalog it can read and fails with its usual typed refusal, the same as for any format it does not support.
- One 32-byte secret authorizes the whole broadcast; HKDF-SHA-256 derives separate AES-128-GCM keys for each physical track and for grouped-frame versus datagram domains. A grouped frame uses the 96-bit nonce `uint64_be(group) || uint32_be(frame)`; a datagram uses its sequence with frame zero under the datagram domain. Empty AAD: every immutable end-to-end field is in the HKDF info or the nonce.
- Within an instance, group and datagram sequences are allocated monotonically and frames are numbered in write order, which is the whole reuse rule. There is no ciphertext retention or retransmission API: relays forward bytes unchanged, and an application that needs to resend encrypts nothing twice because it never gets the same identity twice.
- Per-key limits are `2^24` AEAD invocations (failed opens included) and `2^36` plaintext bytes. The generation handle owns each key's accounting, shared by every track and subscription through it and never evicted while the application holds it; binding the epoch again starts fresh, since remembering retired epochs would be state a relay grows (maintainer, 2026-10-24). Rejected: a credential-level registry of live generations, and refusing to re-bind a released epoch. Datagram plaintext is capped at `1200 - 24 - 16` bytes against the widest moq-lite header; a publisher cannot see the Subscribe ID each hop encodes, so the library does not pretend to budget it.

### Library shape

The Rust and TypeScript cores expose the same surface, and nothing else:

- `Credential { context, kid, secret }` accepts the application-owned secret. The application generates and distributes it over its authenticated channel; the library does not mint a secret it cannot return. `credential.path(semantic)` derives the epoch-free opaque broadcast name. Credential is cheap to clone, never serializes the secret, and redacts it from `Debug`.
- A core produces only under an epoch it minted itself (decided 2026-10-08). Only `credential.produce()` mints a producing generation, and it takes no epoch: it mints a fresh UUIDv7 and returns a `generation::Producer` owning `epoch()` for the route and `name(semantic)` for opaque track names. `credential.consume(epoch)` binds a discovered epoch as a consume-only `generation::Consumer` with `name(semantic)`. The two are separate types, so producing under a discovered or shared epoch does not compile (names decided 2026-10-11 in [#5313](https://github.com/moq-dev/moq/pull/5313)). Clones of a producing generation share publisher claims so reopening a track cannot reset its nonce counters.
- Applications only handle semantic track names (maintainer, 2026-10-17). `broadcast::Producer` wraps a `moq-net` broadcast under a producing generation and `broadcast::Consumer` wraps one under a consuming generation; each derives the physical name, so only the CDN sees physical names. `generation` no longer opens tracks itself. The broadcast wrappers merged in [#5876](https://github.com/moq-dev/moq/pull/5876).
- The wrappers implement the [moq-net traits](/quest/m2/net-wrap/README.md) at the broadcast, track, group, and frame levels, datagrams included (decided 2026-10-20, maintainer, because one encrypted broadcast then works with every writer and reader). moq-mux, moq-json, hang, and the CLI publish and consume an encrypted broadcast unchanged, under semantic names, and every AEAD call still has the canonical physical name and transport identity. The e2ee mux bridge ([#5822](https://github.com/moq-dev/moq/pull/5822)) is closed and superseded.
- Errors are the draft's typed codes plus the transport's. Nothing catalog-, hang-, or MSF-shaped lives here: a catalog is a track under a derived name. The catalog owner sets logical `track::Info.compress`; the [e2ee wrapper](/quest/m2/net-wrap/e2ee.md) compresses before AEAD and authenticates the logical compression metadata.
- Stateless `seal`/`open` primitives with caller-chosen identities, HKDF labels and info builders, raw key bytes, and process-global claims are not public. The vectors are tested inside each core.

### Application and platform shape

- Deterministic secret-derived physical names hide catalog, codec, role, quality, timeline, and custom-track semantics. The decrypted catalog keeps every semantic track name, extensions included, and authorized clients derive each physical name from it (maintainer, 2026-10-17), so neither side rewrites the catalog. Every catalog representation is encrypted; Rust publishers must not emit a plaintext MSF catalog.
- A platform that forwards and meters protected bytes must never preview, record, archive, transmux, transcode, transcribe, compose, or inspect them, rejecting those paths before opening a processing session or writing product state. Applications needing those operations terminate E2EE outside the platform. A platform classifies protected broadcasts by its own credential or product state, never by name; the moq.pro (downstream) exclusion classifier and dashboard work stay downstream.
- The first proof covers browser TypeScript and native Rust publication and playback in both directions, with grouped audio and video over both moq-lite and MoQ Transport. Shared vectors cover groups and moq-lite datagrams. JavaScript carries MoQ Transport datagrams since [#4979](https://github.com/moq-dev/moq/pull/4979), so the proof covers them too.

### Dynamic names

Physical names are reversible (decided with the maintainer on 2026-10-24 in [#6623](https://github.com/moq-dev/moq/pull/6623)). A physical track name is a 12-byte nonce, the first 12 bytes of an HMAC-SHA-256 of the semantic name padded to a multiple of 16 bytes, followed by the AES-128-GCM encryption of that padded name under that nonce, with both keys derived from the generation (the draft's Key Derivation section). It needs only HMAC-SHA-256 and AES-128-GCM, so it runs on aws-lc-rs, graviola, and WebCrypto with no new dependency (maintainer, 2026-10-24, over AES-GCM-SIV, which graviola lacks). A publisher opens a requested name and recomputes its nonce to recover its semantic name, so the e2ee `requests()` answers undeclared names through the [moq-net traits](/quest/m2/net-wrap/README.md) unchanged, and a name that does not open is answered as missing with no state kept. Equal names under one generation still share one physical name, so relays dedupe and cache them, and names stay unlinkable across epochs. AEAD keys bind the semantic name. The cost is that a relay learns each name's length rounded up to 16 bytes: every name up to 15 bytes looks alike, and a longer one leaks its length to within 16 bytes (maintainer, 2026-10-24, over power-of-two buckets that leak less but can double a long name). Broadcast paths stay one-way: e2ee wraps no origin, so nothing requests an undeclared protected broadcast.

Rejected:

- Declared names only (`reserve_track` over an enumerated universe): it cannot serve an open-ended name such as a moq-stats prefix or presence query, so the e2ee `requests()` could not mirror moq-net.
- A name-directory track: a directory the publisher writes is the catalog again and still needs every name in advance. One the subscriber writes requires viewers to publish, links each request to a visible subscriber path, and races the subscription it serves.
- Requests that carry an encrypted semantic name beside a one-way name: it needs a moq-lite and moq-transport field that every relay forwards, and a moq-net `track(name)` signature change that breaks the trait mirror. The relay forwards one coalesced subscriber's field, so a relay-authorized party without the secret could race a bad one and get the name refused for everyone. Its field leaks the same rounded length.
- One-way names for declared tracks and reversible ones for dynamic tracks: one semantic name would have two physical names, so it would not always mean the same content, and a subscriber cannot know which one the publisher serves.

The broadcast wrappers merged in [#5876](https://github.com/moq-dev/moq/pull/5876) with `reserve_track`, which still serves declared names; [rust-publish](/quest/m2/e2ee/rust-publish.md) needs nothing more (maintainer, 2026-10-19).

Before `@moq/e2ee` is first published, add its entrypoints to the API
Extractor declaration checks, which analyze only published packages since
[moq-dev/moq#6794](https://github.com/moq-dev/moq/pull/6794).

## Required

- [JS undeclared track names](/quest/m2/e2ee/js-undeclared-names.md) -
  `@moq/net` exposes a broadcast request handler and `@moq/e2ee` serves
  undeclared names through it
- [Rust protected publisher seams](/quest/m2/e2ee/rust-publish.md) - Rust
  publishers emit encrypted catalogs and no plaintext semantic catalog in
  E2EE mode, through the moq-net traits
- [Encrypted consumer shape](/quest/m2/e2ee/consumer.md) - one consumer shape, the producer's mirror, shared by browser and CLI (moq-ffi is out of scope for this epic)
- [Encrypted browser components](/quest/m2/e2ee/browser.md) - browser publish
  and watch components handle encrypted audio and video without leaking
  credentials into browser-owned surfaces
- [Encrypted native CLI](/quest/m2/e2ee/cli.md) - the native CLI publishes and
  plays encrypted media without exposing credentials through process metadata
  or logs
- [Cross-language encrypted proof](/quest/m2/e2ee/interop.md) - browser
  TypeScript and native Rust exchange encrypted media both ways, with relays
  forwarding and metering blind

## Closes

- [#3023](https://github.com/moq-dev/moq/issues/3023) - close this issue when the quest finishes

## Related

- [archive](/quest/m1/archive/README.md) - protected broadcasts are deliberately outside recording and replay formats
- [Wrapper traits over moq-net](/quest/m2/net-wrap/README.md) - the traits the e2ee wrappers implement, so every writer and reader takes an encrypted broadcast
