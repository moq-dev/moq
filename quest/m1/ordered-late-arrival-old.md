# [M] A key-order reader passes a group that arrived late but within its budget

## Goal

A key-order reader (`track::Ordered`) hands out every group that arrives within its
`delay.max` budget, however late it lands behind a newer one. A group that arrives after
a newer group was handed out is no `Old` gap unless the budget gave up on it.

## Plan

Decided 2026-11-23 (maintainer, /quest-run): keep.

Found on [moq-dev/moq#9422](https://github.com/moq-dev/moq/pull/9422).
In 2 of 40 instrumented runs of that PR's relay repro (`moq import ts` through a relay
with `cache.headroom = "100%"` to `moq export fmp4`, `--max-delay` 500 ms), the export
received group 5383333 11 ms after 5483333, and its `Ordered` reader at once reported
5383333 as an `Old` gap: the inner cursor had handed out 5483333, and `Cursor::note_late`
then recorded the late arrival given up `Old`. The 11 ms was far inside the 500 ms
budget, and no drop came from the relay. It recurred once in 80 later instrumented runs:
at 020866cf72 the export received a relay's re-fetched copy of 6283333 and `note_late`
named it `Old` the next millisecond.

Next: probe `poll_owed` and `poll_next_with` in that repro to find why the cursor passed
the owed key (`TrackState::owed` judging it accounted for, or the cache floor),
and add a unit test with that ordering. Mirror in `js/net` if its ordered reader shares
the rule.
