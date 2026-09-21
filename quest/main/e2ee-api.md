# [M] Rust E2EE core on moq-e2ee-00

## Goal

`rs/moq-e2ee` implements profile `moq-e2ee-00` with the library shape in the
[questline](/quest/next/e2ee/README.md): epoch-scoped generations, monotonic
identities, and no state that has to survive a publisher instance.

Reshape the 0.0.x API in place without compatibility aliases. Check publication
status separately before replacing a wire profile.

## Plan

- Replace `Credential::new(profile, context, generation, kid, secret)` with `Credential::new(Config { context, kid, secret })`, accepting an application-owned 32-byte secret. Remove `Credential::generate`: it hides the generated secret and cannot provision another process or device. Drop `profile`, `Pin`, `check_pin`, `prk_bytes`, `name_info`, `key_info`, and `key_bytes` from the public surface; the crate is the profile and the vectors run in-crate.
- Add `Credential::path(semantic) -> Path`, the profile's epoch-free opaque broadcast name, and `Generation` from `credential.generation(epoch)`. It owns `name(semantic) -> Name`, `produce(moq_net::track::Producer) -> track::Producer`, `consume(moq_net::track::Subscriber) -> track::Consumer`, and `Epoch::mint()` returning a lowercase UUIDv7 via the `uuid` crate (`v7` feature, added to `[workspace.dependencies]`). `Name` replaces `PhysicalName`; `Epoch` is a validated path segment (nonempty, no `/`, at most 65535 bytes).
- Delete `Publication` and its process-global generation set, `retransmit_datagram`, `datagram_ciphertext`, `group::Producer::ciphertext`, the producer-side datagram retention map, `GroupWindow`, `set_subscribe`, `datagram_payload_limit`, `varint_len`, and the `catalog` module. Keep `TrackKey`, `protect`, `open`, and `nonce` crate-private.
- Clones of a generation share per-track publication claims. Reopening the same physical track must not reset its nonce counters; mint a fresh epoch for a new publisher instance. Keep this state inside the generation, never in a process-global registry.
- `track::Producer` allocates sequences monotonically and refuses `create_group` or `insert_datagram` below the next sequence with `Reuse` within the claimed track. Frames are numbered by write order. The datagram plaintext cap is the constant `MAX_DATAGRAM_PLAINTEXT` (1160).
- `track::Consumer` keeps the datagram sliding window as a 1024-bit bitmask below the greatest opened sequence and marks only after a successful open. Preserve the receive limits and terminal-state behavior covered by [Receive failure](/quest/next/e2ee/receiver-failure.md); that implementation-only fix is independently landable.
- Swap `include_str!("../../../drafts/moq-e2ee-01.json")` for the `-00` vectors, then delete `drafts/moq-e2ee-01.json` and `drafts/moq-e2ee-01.ts`; `just drafts check` globs the remaining generator. Keep the lifecycle tests the draft requires: monotonic allocation, exhaustion with failed opens counted, bounded datagram suppression, and a new epoch authenticating while the old keys do not.
- Update `doc/lib/rs/index.md`, the crate README, and the changelog to the new surface. Keep raw key material, HKDF helpers, catalog policy, and duplicate aliases out of the public exports; expose only the limits applications use to size payloads.

Public API: breaking reshape of moq-e2ee 0.0.1. Wire/interoperability: the
implemented `moq-e2ee-01` numeric generation and HKDF labels become the
draft's `moq-e2ee-00` epoch-based derivation, changing names, keys, and
ciphertext compatibility. Do not describe this as no wire change merely
because the target draft already exists. Verify publication status before
implementation and preserve any actually published profile as required by
the repository's wire-compatibility rule.

## Related

- [Receive failure](/quest/next/e2ee/receiver-failure.md) - key usage accounting and waking terminal reads, without an API change
- [TypeScript E2EE core](/quest/next/e2ee/typescript.md) - mirrors this surface name for name
