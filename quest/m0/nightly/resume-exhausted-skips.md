# [L] A resumed read ends once every group of its window is read or accounted for

## Goal

A resumed reader's bounded read ends exactly when every group of its window
was handed out, dropped, skipped, or given up, and never before. Neither a
group the local floor or cap keeps out of an earlier copy's placement, nor a
group the publisher skipped, leaves the read pending or ends it early, in Rust
(`rs/moq-net/src/model/resume.rs`) and JS (`js/net/src/resume.ts`) alike.

## Plan

Found on [moq-dev/moq#9278](https://github.com/moq-dev/moq/pull/9278)'s
resume model sweeps, and reproduced on `main` at `b4496b1207`. The reader's
`exhausted` judges groups the local floor or cap keeps out only under the
serving copy's resume point, from `Delivered` alone, and hands everything
above it to the serving copy's `exhausted_for`. Two cases break that split:

- An earlier copy placed a range lower than the serving copy places it, so
  a kept-out group between the two placements is owed but neither side
  checks it: the read ends early. The model case, failing on `main` in both
  languages with `missing groups [0]`:
  `join r2 unordered [8,9]; bounds r2 [1,9]; write 0/0 next; up 0/1`.
- Overlapping ranges `8..=8` and `0..=8`, where an
  earlier copy placed the first at 6 and the serving copy places it at 8: a
  group 7 the publisher skipped is missing from `Delivered`, so a check from
  `Delivered` alone keeps the read pending, though the serving copy accounted
  for the skip through the second range. On `main` the same holds for a
  skipped group under the resume point that the local floor keeps out.

That PR tried three boundaries for the split (the resume point, the serving
copy's lowest own placement, each range's own placement); each left a case
on one side or the other. Instead, judge the whole window in one coverage
check: `Window::covered_by` (`rs/moq-net/src/model/subscription/window.rs`)
with an `Accounted` that joins the logical readers' `Delivered` with the
serving copy's skips and give-ups, leaving the serving copy only the track's
end. Mirror it in JS beside `hooks.exhaustedFor`.

Add both cases above to each language's model `regressions`, and unit tests
beside them. Keep a single-route read over a skip under the local floor
ending once the rest is read (its first attempt broke it).

Public API: none. Wire: none.
