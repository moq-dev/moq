# End-to-end encrypted MoQ

## Goal

TypeScript and Rust publishers and subscribers interoperate over encrypted MoQ broadcasts while every relay, cache, and control plane remains unable to recover content.

Every application payload and semantic track name is confidential and authenticated. MoQ still exposes the outer broadcast path, opaque physical track names, group and frame structure, timestamps, sizes, and traffic patterns; padding and metadata-flow confidentiality are out of scope.

## Plan

### Security boundary

- The CDN, relay, cache, recorder, and any platform control plane are actively untrusted for content. Authorized publisher and subscriber endpoints are trusted; sender authenticity against another endpoint holding the broadcast secret is not a first-version goal.
- Applications distribute credentials over their own authenticated channel. MoQ announcements, catalogs, paths, relay authorization, and platform APIs never distribute or authenticate content keys.
- E2EE is an explicit per-broadcast mode with no plaintext fallback. Authentication failure ends a grouped track with a typed error; a bad datagram is dropped with a typed event.
- Receivers keep a bounded at-most-once window as operational duplicate suppression. The AEAD identity and generation rules provide the security boundary; relays may still delay, reorder, suppress, or replay ciphertext outside a receiver's retained window.

### Keys and object identity

- The application supplies an immutable `(broadcast context, generation, KID, broadcast secret)` credential. The secret is 32 bytes from a cryptographically secure random generator, never a password or other guessable input. One secret authorizes the whole broadcast; HKDF-SHA-256 derives separate AES-128-GCM keys for each physical track and for grouped-frame versus datagram domains.
- Rotation starts a new broadcast generation. A generation and KID never change in place, and every publisher restart or replacement that can reset transport sequence numbers requires a new generation. The application pins the authorized generation, so a relay-replayed catalog or announcement is never a freshness authority.
- A grouped frame uses the 96-bit nonce `uint64_be(group ID) || uint32_be(frame ID)`. A datagram uses its 64-bit sequence with frame ID zero under the separate datagram key domain. AES-GCM's internal block counter is not the frame ID; implementations use the standard AEAD API and fail at the strictest interoperable identity bound. Current TypeScript rejects before an ID exceeds `Number.MAX_SAFE_INTEGER`, and every implementation rejects before the 32-bit frame ID or AEAD invocation limit is exhausted.
- Retransmission and cache replay reuse the original ciphertext. Re-encrypting different bytes at an existing `(credential, track, domain, group, frame)` identity is forbidden. No random per-group salt or per-frame KID header is carried.
- The profile authenticates its version and every immutable property that has one canonical end-to-end value in both moq-lite and MoQ Transport. The broadcast context, generation, KID, full physical track name, domain, group, and frame are bound through derivation or the nonce; rewritten timestamps and mutable routing properties are excluded.

### Application and platform shape

- Deterministic secret-derived physical names hide catalog, codec, role, quality, timeline, and custom-track semantics. Authorized clients derive the encrypted catalog track name, then learn the remaining opaque names from its decrypted contents. Every catalog representation is encrypted; Rust publishers must not emit a plaintext MSF catalog.
- Protected broadcasts use an outer `.e2ee` suffix such as `foo.hang.e2ee`, deliberately not `.hang`. The suffix is an untrusted application convention for exclusion and discovery, not a key identifier or cryptographic assertion.
- A platform that forwards and meters protected bytes must never preview, record, archive, transmux, transcode, transcribe, compose, or inspect them, rejecting those paths before opening a processing session or writing product state. Applications needing those operations terminate E2EE outside the platform. The moq.pro (downstream) exclusion classifier and dashboard work build on that rule and stay downstream.
- The first proof covers browser TypeScript and native Rust publication and playback in both directions, with grouped audio and video over both moq-lite and MoQ Transport. Shared vectors cover groups and moq-lite datagrams; MoQ Transport has no datagram delivery.

The profile starts from IETF Secure Objects where its object model maps exactly, specifies the moq-lite and datagram bindings it does not cover, and records intentional differences from SFrame and the experimental `moq-secure` format linked from [#3023](https://github.com/moq-dev/moq/issues/3023).

## Quests

- [Encryption profile](/quest/m2/e2ee/profile.md) - a versioned MoQ E2EE
  profile: payload protection, identity binding, key derivation, failure
  behavior, and shared vectors
- [Explicit datagram insertion](/quest/m2/e2ee/datagram-insert.md) - name and
  shape the explicit-sequence datagram API as insertion in both languages,
  reserving append for automatic allocation
- [TypeScript E2EE core](/quest/m2/e2ee/typescript.md) - a reusable TypeScript
  layer protecting groups, datagrams, catalogs, and track identities, without
  putting keys in `@moq/net`
- [Rust E2EE core](/quest/m2/e2ee/rust.md) - the Rust twin of that layer,
  giving native peers the same context-aware contract
- [Rust protected publisher seams](/quest/m2/e2ee/rust-publish.md) - Rust media
  and catalog publishers accept opaque physical names and emit no plaintext
  semantic catalog in E2EE mode
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

- [#2277](https://github.com/moq-dev/moq/issues/2277) - close this issue when the quest finishes
- [#3023](https://github.com/moq-dev/moq/issues/3023) - close this issue when the quest finishes

## Related

- [archive](/quest/m1/archive/README.md) - protected broadcasts are deliberately outside recording and replay formats
- [HLS playable](/quest/m1/hls-playable.md) - stock HLS and DASH require plaintext media and exclude `.e2ee` broadcasts
