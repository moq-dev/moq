# [L] Signed priority

## Goal

Every priority in the API is an `i8`, higher first, with 0 as the unset
midpoint: `track::Info`, `Subscription`, and `group::Fetch` in Rust, their
JS counterparts, moq-ffi, libmoq, and every wrapper. Nobody has to know that
127 is the middle of a `u8`, the default the moxygen line ships. The wire
stays a byte.

## Plan

Decided with the maintainer:

- moq-lite carries `p + 128` (flip the top bit). The mapping is one-to-one
  and keeps order, so a given byte means what it means today; only the API
  number changes. The default becomes byte 128.
- IETF carries `128 - p`, saturating. An unset priority goes out as the
  draft's usual 128, and an absent IETF priority decodes to 0. The cost is
  that i8 -128 and -127 share byte 255, and IETF bytes 0 and 1 both decode to
  127. Pin both ends in tests.
- hang's built-in priorities move above 0, so hang media outranks a track that
  never set one. Something like catalog 40, text 30, audio 20, video 0; the
  spacing is the implementer's call. Rust and JS keep matching values.
- A zeroed libmoq `moq_track_info` then means the default, which retires the
  need for a `priority_present` flag.

Changing published `u8` fields to `i8` is an API break in every language, so
this lands on `dev`. Look for anything that does arithmetic on priority
(the lite send queue, JS send-order packing, the bandwidth allocator, the
relay's max-of-subscribers) and keep its ordering, not just its type.

Report the wire impact in the PR: none in format, but the default byte on
moq-lite moves again.

## Related

- [Scope track priority](/quest/m1/track-priority-scope.md) - which streams a priority competes with, not its type
