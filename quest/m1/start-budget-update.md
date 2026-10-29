# [S] A tightened SUBSCRIBE keeps the start's budget honest

## Goal

A relay copy remembers the `max_delay` that was in effect when its
publisher resolved SUBSCRIBE_START, not only the one sent at SUBSCRIBE. If a
SUBSCRIBE_UPDATE tightens the delay before the start is picked, a later
reader whose delay is looser than the tightened one, but no looser than the
original, still fills the history between the two starts. Released wires are
unchanged.

## Plan

Decided 2026-10-29 (maintainer, `/quest-plan` of moq.pro's announce ramp
loss follow-ups): its own quest in m1, rather than waiting on
[Drop set](/quest/m1/drop-set.md) or
[Delete the old start bookkeeping](/quest/m1/subscribe-ranges/start-bookkeeping.md),
since it fixes released lite-06 behavior.

Found by review of [#7168](https://github.com/moq-dev/moq/pull/7168): the
lite subscriber sets `requested_max_delay` once, at SUBSCRIBE
(`rs/moq-net/src/lite/subscriber.rs`). `Misses` then gives up every group
below the start for any reader whose budget is no looser than that value.
After a tightening update, the start was resolved under the tighter budget,
so the recorded looser one makes some readers skip history they could have
filled. It loses history only in that window; it does not bring back #7168's
over-fetch.

Record the budget the start was actually resolved under. SUBSCRIBE_START
carries a group, not which update it was picked under, and updates can cross
it in flight, so the budget current when START arrives is not enough: an
update sent after the publisher picked the start would be recorded in its
place, too loose after a widening or too tight after a tightening. When the
subscriber cannot tell which of its sent budgets the publisher applied,
record the loosest of them. That keeps today's history loss in the
ambiguous window but never brings back the over-fetch.

Add integration tests in `rs/moq-net/tests/integration`, direct and through
a relay: subscribe, tighten before the start, then a second reader widens
partly, which fails on `main` and passes with the fix; and a tightening and
a widening that each cross an already-picked START, where the recorded
budget must never be tighter than the one the start was picked under.

Open question for whoever starts this: which lite-06 ordering, if any, tells
the subscriber the publisher applied a tightening before picking the start.
Without one, every tightening sent before START arrives stays ambiguous, the
loosest-budget rule records the SUBSCRIBE budget as today, and the Goal is
out of reach on the released wire. Then propose rewriting this quest to a
wire change for a later draft, or deleting it.

## Related

- [Drop set](/quest/m1/drop-set.md) - records the budget each drop was judged under, the same idea for drops
- [Delete the old start bookkeeping](/quest/m1/subscribe-ranges/start-bookkeeping.md) - moves where a subscription starts into the model, which should carry this field
