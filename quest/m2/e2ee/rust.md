# [M] Rust E2EE core on moq-e2ee-00

## Goal

`rs/moq-e2ee` implements profile `moq-e2ee-00` with the library shape in the
[questline](/quest/m2/e2ee/README.md): epoch-scoped generations, monotonic
identities, and no state that has to survive a publisher instance.

The crate is unpublished, so this is a reshape in place, not a compatibility layer.

## Plan

- Replace `Credential::new(profile, context, generation, kid, secret)` with `Credential::new(Config { context, kid, secret })` and `Credential::generate(context, kid)`. Drop `profile`, `Pin`, `check_pin`, `prk_bytes`, `name_info`, `key_info`, and `key_bytes` from the public surface; the crate is the profile and the vectors run in-crate.
- Add `Generation` from `credential.generation(epoch)`. It owns `name(semantic) -> Name`, `produce(moq_net::track::Producer) -> track::Producer`, `consume(moq_net::track::Subscriber) -> track::Consumer`, and `Epoch::mint()` returning a lowercase UUIDv7 via the `uuid` crate (`v7` feature, added to `[workspace.dependencies]`). `Name` replaces `PhysicalName`; `Epoch` is a validated path segment (nonempty, no `/`, at most 65535 bytes).
- Delete `Publication` and its process-global generation set, `retransmit_datagram`, `datagram_ciphertext`, `group::Producer::ciphertext`, the producer-side datagram retention map, `GroupWindow`, `set_subscribe`, `datagram_payload_limit`, `varint_len`, and the `catalog` module. Keep `TrackKey`, `protect`, `open`, and `nonce` crate-private.
- `track::Producer` allocates sequences monotonically and refuses `create_group` or `insert_datagram` below the next sequence with `Reuse`; that is the only reuse rule. Frames are numbered by write order. The datagram plaintext cap is the constant `MAX_DATAGRAM_PLAINTEXT` (1160).
- `track::Consumer` keeps the datagram sliding window as a 1024-bit bitmask below the greatest opened sequence, marks only after a successful open, counts failed opens against the key, and aborts the inner subscriber once a grouped frame fails authentication so a caller cannot keep polling a dead track.
- Swap `include_str!("../../../drafts/moq-e2ee-01.json")` for the `-00` vectors, then delete `drafts/moq-e2ee-01.json` and `drafts/moq-e2ee-01.ts`; `just drafts check` globs the remaining generator. Keep the lifecycle tests the draft requires: monotonic allocation, exhaustion with failed opens counted, bounded datagram suppression, and a new epoch authenticating while the old keys do not.
- Update `doc/lib/rs/index.md`, the crate README, and the changelog to the new surface. No wire change; the draft already carries the profile.

## Related

- [TypeScript E2EE core](/quest/m2/e2ee/typescript.md) - mirrors this surface name for name
