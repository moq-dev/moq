# [XS] moq-archive cancels an abandoned lookup's store read

## Goal

A `track::Fill` exposes a public signal for "no reader waits on this fill any more", and moq-archive's handler uses it to stop the store read of a time lookup whose readers left, so time-range readers that churn don't leave slow stores serving reads nobody takes.

## Plan

Follow-up of [moq-dev/moq#7310](https://github.com/moq-dev/moq/pull/7310), which resolves archive time ranges through a fill lookup: a fill over `0..reach` carrying the unresolved time in `Fill::time()` and owing no group. moq-archive used `fill.unused()` to stop a slow store read once nobody waited, but [moq-dev/moq#7036](https://github.com/moq-dev/moq/pull/7036) removed it, and a lookup owes no group, so `unwanted(sequence)` can't stand in. #7310 dropped lookup cancellation by maintainer decision (2026-11-01, "no new moq-net API" in that PR), so an abandoned lookup now finishes its read and answers nobody. Group fills still stop on `unwanted(sequence)`. This waits on #7310 landing.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01 /quest-afk run): m2, adding the public withdrawal signal #7310 deferred, since wasted store reads under churn are a real cost for slow stores.

- `Fill::poll_withdrawn` exists but is crate-private (`rs/moq-net/src/model/track/fill.rs`, about line 759, and async-only). Make it, or a `withdrawn()` future over it, public, rather than #7310's other alternative of `time()` turning `None` once withdrawn, which would overload a value with a lifecycle.
- moq-archive races its lookup's store read against it and drops the read on withdrawal.
- Test: a lookup whose only reader leaves mid-read stops its store GET (a slow mock store counts completed reads).
- Mirror it in `@moq/net` only if a JS handler needs it; none does today.

Public API: a public withdrawal signal on `moq_net::track::Fill`. Wire: none.

## Related

- [A failed archive time lookup fails the subscription](https://github.com/moq-dev/moq/pull/7440) - planned in that open PR as `quest/m2/archive-time-followups.md`: a lookup refusal on `Fill`, the other half of a lookup's lifecycle; whichever lands second shares its `Fill` surface
