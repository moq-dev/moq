# [M] Bare track handles are lazy

## Goal

A bare Rust `track::Consumer` (no query, no subscription) sends nothing on the
wire and counts as no demand until it queries or subscribes, matching JS. The
publisher sees demand only for queries and subscriptions.

## Plan

Decided 2026-10-08 in a `/quest-plan` round on #5053's open options. It
reverses #5053's option 1 ("a held handle is interest end to end") in favor
of its option 3 ("lazy, like JS").

- Query interest becomes visible at the front: a held query is counted on the
  track and mirrored onto the copy, so a query still reaches upstream. That is
  the part of #5053's option 2 that option 3 needs.
- Local `Demand` stops counting an idle bare handle. That also closes #5053's
  documented gap (after a subscription ends with a bare handle held, the
  publisher saw `unused` while local `Demand` read `used`), so remove that
  comment where `ServeLoop` releases the TRACK
  (`rs/moq-net/src/lite/subscriber.rs:4528` at `11ee77d5d`).
- Update the `track::Consumer` rustdoc and `doc/lib/rs/moq-net.md`.
  moq-transcode already stops holding idle source handles (#5053).

Rejected: option 2 alone (a demand pulse for bare handles, with the gap still
open), and keeping #5053's behavior.

Tests: a held bare handle sends no TRACK and leaves the publisher's demand
unused; a query on it opens TRACK and reaches upstream through a relay; after
a subscription ends with the handle held, demand reads `unused` both locally
and at the publisher.

Public API: `track::Consumer` demand semantics change (documented). Wire:
none.

## Related

- [SUBSCRIBE goes out with TRACK](/quest/m1/pipeline-requests/subscribe.md) - builds on this demand model
