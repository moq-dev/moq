# [L] Max Age is a Live range's staleness budget

## Goal

On lite-07-wip, a Live `Start` carries both its join lag and its staleness
budget, `Live(min, max)`. Only a group reached through a Live range can expire
from delivery for staleness, and its age is measured from the live edge, which
is what a Live range follows. A Time, Position, or Oldest/None range is a floor
for history: its groups are delivered in full, bounded only by what the
publisher retains (`Publisher Max Age`). A relay serving one DVR reader and
many live readers asks upstream for each reader's own budget, so the history
reader no longer makes every live reader's upstream carry stale groups.

Non-goals: lite-05/06 and moq-transport, whose encodings moq-net keeps mapping.

## Plan

Decided 2026-11-21 (maintainer, on
[moq-dev/moq#9316](https://github.com/moq-dev/moq/pull/9316#issuecomment-7072736797)):

- ✅ `Live(min, max)`: `max` is a per-range staleness budget measured from
  the live edge. Time, Position, and Oldest/None ranges are floors for history
  and never expire for staleness. The subscription-level `Subscriber Max Age`
  is removed from SUBSCRIBE and SUBSCRIBE_UPDATE, and from FETCH, where it only
  tightened the publisher's retention. Rejected: one subscription Max Age that
  applies only to groups reached through Live ranges (a relay still asks for
  the largest across its readers); leaving #9316's shape.
- ✅ Planned as its own quest after #9316 lands, not folded into it.

Why: since #9316, Max Age is one number per subscription with two jobs. It
expires non-latest groups from delivery, and it floors how far back a start
reaches. Both are measured from the live edge, the newest frame the publisher
holds. A history reader plays at its own playhead, which the publisher never
sees, so ages from the live edge expire exactly the history it asked for. The
draft works around this ("an ascending subscriber SHOULD raise its Subscriber
Max Age"). A relay must ask upstream for the largest Max Age among its readers
(the draft's "under a `Subscriber Max Age` at least as large as the
subscriber's"), so one history reader inflates every live reader's upstream.

Work out in the PR:

- A group inside several Ranges takes the largest budget among them, and none
  (never stale) if any non-Live Range covers it. The publisher still delivers
  each group once.
- SUBSCRIBE_UPDATE gains a way to change a Live Range's `max` in place.
  Retiring and re-adding the Range would re-join at a moved live edge, and an
  adaptive player changes only `max` while it plays (#9316's decision 10).
- A None `Start` no longer has a Max Age to reach back by, so decide whether
  it becomes "the oldest group the publisher holds" (as Oldest) or is removed in
  favor of `Live`.
- The relay's copy: per-Range budgets replace the "at least as large as the
  subscriber's" rule for placing a join and for a Time `Start` proven by
  `Range Start` (draft's "smallest `Subscriber Max Age` asked between adding that
  Range and its answer").
- API, mirrored in Rust, js/net, and moq-ffi: `track::Start::Live` carries
  `{ min, max }`, and `Delay` goes away from Rust and JS. moq-ffi's
  `MoqDelay { min, max }` keeps its shape and maps one-to-one onto it.
- The draft, its changelog, and `doc/concept/moq-lite.md`.

Run `/verify-subscribe` if the change reaches how the publisher judges stale
(OLD) drops, and `just test interop --all` for the wire change.

## Waiting

- moq-dev/moq: [#9316](https://github.com/moq-dev/moq/pull/9316) merges (the
  Live anchor this extends). Check with `gh pr view 9316 --json state`.
