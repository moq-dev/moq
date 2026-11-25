# [M] A reader that still wants a group reset to make room fetches it

## Goal

A reader whose budget still wants a group, such as an ordered reader with no
max age, gets that group whole even when the publisher reset its Group
Stream to make room for a higher-priority one. Today it does not: the
reader's group ends with `TOO_FAR_BEHIND` after the frames that arrived, and
the next group follows, on every moq-lite version. Rust and `@moq/net`
behave alike.

## Plan

Found by [moq-dev/moq#8965](https://github.com/moq-dev/moq/pull/8965), which
made the reset to make room a final drop on lite-07 too and left a reader to
fetch the group itself. `test/stream-credit` does that fetch by hand in the
tab (`test/stream-credit/src/main.ts`), and every fetched group comes back
whole, so the publisher side is done.

Where the gap is:

- `resume::Recover` (`rs/moq-net/src/model/resume.rs`, the verdict paths in
  `Recover::poll_serving` and the drop recording) ends a group with a route's
  non-retryable error, and `Error::retryable` (`rs/moq-net/src/error.rs`)
  excludes `TooFarBehind`. The
  `a_verdict_is_never_asked_of_its_own_route` test pins that.
- `js/net/src/resume.ts` `routeFailed` does the same.

Recommended: a reader fetches a group it still wants whatever code the drop
or reset carried, rather than matching on `TooFarBehind`. A drop judges the
route's subscription, not whether the group exists, as the comment in
`Recover::poll_serving` already says. Only a verdict the reader's own budget would
reach, such as a stale (OLD) drop judged against the budget it sent, ends
the group. Since [moq-dev/moq#8908](https://github.com/moq-dev/moq/pull/8908)
a lite-07 subscriber already asks again, on a request of its own, for an OLD
drop a wider reader still wants (`stale_budget` in
`rs/moq-net/src/lite/subscriber.rs`); a drop to make room can take the same
path. Updated 2026-11-24 (maintainer, /quest-plan of the subscribe/fetch
audit): on lite-07, `FETCH` is removed and `SUBSCRIBE_DROP(OLD)` carries its
judged `Max Age` (replacing `stale_budget`), so re-requesting a preempted
(`TOO_FAR_BEHIND`) group uses a one-shot bounded `SUBSCRIBE`
(`[Position(G, 0), Position(G + 1, 0))` with immediate subscriber `FIN`) rather
than wire `FETCH`. Model the change with the verify-resume skill first, since it
changes which errors end a group.

Tests: `rs/moq-net/tests/integration/stream_credit.rs`
`a_reset_group_is_fetched_whole` drops its manual `fetch_group` and reads
the group whole through the ordered reader; `test/stream-credit` drops the
tab's fetch and asserts the top two tiers read every group whole. Run
`just test interop --all`.

This also gives [takeover](/quest/m3/group-takeover.md)'s "Measure first"
the drop plus bounded SUBSCRIBE baseline it compares a continuation against.

Public API: none. Wire: none.

## Required

- [Remove FETCH from lite-07 and unify into SUBSCRIBE](/quest/m2/lite07-subscribe/remove-fetch.md) - re-requesting a preempted group on lite-07 uses one-shot bounded SUBSCRIBE
- [Per-range priority, order, and judged OLD drops on lite-07](/quest/m2/lite07-subscribe/per-range-policy.md) - judged Max Age on SUBSCRIBE_DROP(OLD) replaces stale_budget

## Related

- [Takeover](/quest/m3/group-takeover.md) - measures this against a continuation before lite-08 brings one back
- [Browser reset reports](/quest/m4/browser-reset-reports.md) - why a browser reader learns of the reset from the drop, not the stream
