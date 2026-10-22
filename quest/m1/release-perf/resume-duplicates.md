# [M] Resume duplicates

## Goal

An ordered resume reader skips a route copy's groups that another copy
already delivered, without consuming them. `origin/resume_overlap/sequence`
costs about one cheap skip per duplicate again.

## Plan

Found by reading code on 2026-10-21 (4.6x at `1r_64g_32c`, confirmed).

- Release: each segment did a non-consuming `poll_seek_group(floor, cap)`, so
  a duplicate below the floor cost nothing.
- Main: each copy has its own `track::Subscriber` (from
  [#4741](https://github.com/moq-dev/moq/pull/4741)), and the `Delivered`
  dedupe comes from [#5446](https://github.com/moq-dev/moq/pull/5446). For
  each group handed out, `resume::Reader::poll_lowest` peeks all 32 copies,
  and [#6019](https://github.com/moq-dev/moq/pull/6019) added peeking every
  copy (its own PR measured about 1.3x of the cost). For each copy whose head
  was already delivered, it then fully consumes the head with
  `sub.poll_next_group`. That runs the meter, the inner `hand_out`,
  `forget_past_budget`, `count_stale`, and builds a group consumer, only to
  discard it.

Fix: in ordered mode, move a copy's cursor up to the floor with a seek that
builds no consumer, meter entry, or `hand_out`, as release did per segment.
Cache each copy's peek across polls and re-peek only the copy that delivered.
#6019 also lists skipping peek-then-take when only one live copy remains; do
that here if it falls out of the same change.

Before fixing, check whether `origin/arrival/1r_64g_32c` regressed by the
same factor. If it didn't, that confirms the lost seek is the cause.
