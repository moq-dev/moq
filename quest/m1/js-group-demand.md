# [M] JS group demand counts only readers that will read

## Goal

In `@moq/net`, a group is wanted only while some reader will still read it,
as in Rust. A copy another reader's sink holds but that reader has passed,
or will never reach, no longer keeps the group wanted. So a received copy's
fill FETCH, and the IETF range fetch, stop downloading a group as soon as no
reader will read it, as Rust's do.

## Plan

Planned 2026-11-04 with the maintainer, from the follow-ups of
[moq-dev/moq#7845](https://github.com/moq-dev/moq/pull/7845), whose fill
ingest stops once nobody holds the group (`untilAbandoned`). Milestone m1
(maintainer).

The gap: a cached group is mirrored into every subscribed sink
(`#mirror` in `js/net/src/track.ts`), and a mirror counts as holding the
group. So a reader reading in arrival order that already passed the group,
or one whose range or budget will skip it, still keeps it wanted, and the
fill keeps downloading. IETF's `RangeFetch.unwanted`
(`js/net/src/ietf/range.ts`) reads the same group demand. Rust judges a fill
by the runs its joins still read (`Fill::wanted` in
`rs/moq-net/src/model/track/fill.rs`: "a reader still reads it, and it was
neither answered nor delivered live since it was asked").

- Match Rust's rule, not a new one: first confirm what Rust counts (a
  join's unread runs, and whether a queued, not-yet-received group counts),
  and write it down in the PR.
- A buffered mirror the reader will still reach keeps counting. Only one
  behind its cursor, or outside what it will read, stops counting.
- Fix it where demand is computed, so the lite fill ingest, IETF range
  fetches and any other consumer of group demand follow the same rule.
  Don't special-case the fill.
- Keep the order with the [drop set](/quest/m1/drop-set.md), which changes
  how a cursor judges a missing group and asks `js/net` to mirror each case.
  Whether a reader "will read" a group depends on that judgment, so build on
  whichever has landed and leave the other a note.
- Tests: a fill FETCH stops once the only remaining mirror belongs to a
  reader that passed the group, and keeps going while a mirror's reader
  still has to reach it. An IETF range fetch stops once its last reader
  passes the requested group.

Public API: none. Wire: none.

## Related

- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - the JS received-copy fills this stops sooner
- [Drop set](/quest/m1/drop-set.md) - each cursor's judgment of what it will still read
- [JS fastest-reader pacing](/quest/m2/js-fastest-reader.md) - also paces by which readers still read a group
