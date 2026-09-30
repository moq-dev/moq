# [XS] IETF drafts 14-16 send no timestamps without units

## Goal

On moq-transport drafts 14 through 16, where TIMESCALE can't be sent, the Rust
and JS publishers write no Timestamp object property, as
`drafts/draft-lcurley-moq-timestamp.md` requires ("A publisher that emits
Timestamps MUST send TIMESCALE").

## Plan

Rust sets `has_extensions: self.timescale.is_some()`
(`rs/moq-net/src/ietf/publisher.rs`), but `Properties::encode` never writes
the TIMESCALE block before draft-17 (`rs/moq-net/src/ietf/properties.rs`), so
drafts 14-16 carry timestamps without units. The Rust subscriber ignores them.
Gate the Timestamp property on a timescale actually sent. Check js/net's
`stamped` flag (`js/net/src/ietf/publisher.ts`) for the same gap.

Before landing, check whether an interop peer on draft-14 reads the Timestamp
property without TIMESCALE. If one does, ask whether to keep it.

Fix the stale `track::Info::timescale` doc (`rs/moq-net/src/model/track.rs`),
which says IETF always falls back to local milliseconds; draft-17 and later
carry TIMESCALE.

Wire: stops sending a property peers can't interpret, on published drafts.
Public API: none. Lands on `main`.
