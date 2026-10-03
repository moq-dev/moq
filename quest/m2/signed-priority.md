# [L] Signed priority

## Goal

Every priority in the API is an `i8`, higher first, with 0 as the unset
midpoint: `track::Info`, `Subscription`, and `group::Fetch` in Rust, their
JS counterparts, moq-ffi, moq-c, and every wrapper. Nobody has to know that
127 is the middle of a `u8`, the default the moxygen line ships. The wire
stays a byte.

## Plan

Decided with the maintainer:

- moq-lite carries `p + 128` (flip the top bit). The mapping is one-to-one
  and keeps order, so a given byte means what it means today; only the API
  number changes. The default becomes byte 128.
- IETF carries `127 - p`. Both mappings are one-to-one over the whole `i8`
  range, with no saturation. An unset priority goes out as IETF 127, not the
  draft's usual 128, and an absent IETF priority decodes to 0, the unset
  default. Pin both ends in tests.
- Invariant, kept from the default-priority change (#4273): one urgency on
  both wires, so the IETF byte is always `255 -` the lite byte. Mapping IETF
  as `128 - p` to hit the draft's 128 would break it, which is why the
  default is one step off the draft there.
- hang's built-in priorities move above 0, so hang media outranks a track that
  never set one. Something like catalog 40, text 30, audio 20, video 10; the
  spacing is the implementer's call. Rust and JS keep matching values.
- A zeroed moq-c `moq_track_info` then means the default, which retires the
  need for a `priority_present` flag.

Changing published `u8` fields to `i8` is an API break in every language. Look for anything that does arithmetic on priority
(the lite send queue, JS send-order packing, the bandwidth allocator, the
relay's max-of-subscribers) and keep its ordering, not just its type.

moq-archive's `Info::priority` follows. Its version-1 `.info` stores the
`u8`, so existing recordings must keep their meaning: store the moq-lite byte
(`p + 128`) or bump the format version, never reinterpret silently.
`doc/concept/moq-lite.md` (the 0..255 knob) and `doc/concept/standard.md`
(IETF 128 maps to 127) move to the new range and mapping.

Report the wire impact in the PR: none in format, but the default byte moves
again, from 127 to 128 on moq-lite and from 128 to 127 on IETF.

Decided in the 2026-09-30 audit: moved to m2. It stays deferred unless it
ships in the same release as the moxygen default change, so the default
byte moves once instead of twice.

## Related

- [Scope track priority](/quest/m1/track-priority-scope.md) - which streams a priority competes with, not its type
- [Hierarchical stream scheduling](/quest/m1/quic/scheduler.md) - reworks the same priority arithmetic in the transport
