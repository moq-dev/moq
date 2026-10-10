# [S] A followed path reports a gap that ended its request

## Goal

`origin::Consumer::follow` reports a gap that ended the request (an `End`
then `Start`, or a `Restart`) even when the next serving route is a covering
prefix with the same epoch, so `moq play` never misses a handoff while it is
still draining the old run.

## Plan

Found by the OpenAI review of #5154 (2026-10-10), which added `follow` in
moq-net and `@moq/net`. A same-epoch handoff from the exact route to a
covering prefix, after a gap that ended the old request, reaches the Rust
follower as an `Update`, because the announce cursor delivers by prefix and
the follower never sees the gap. If that `Update` arrives while `moq play` is
draining the old run, playback never restarts. JS sees the gap and is
correct.

Decided 2026-10-10: merge #5154 and fix the cause in moq-net here, in m0
because the epoch line gates the release. Rejected: re-requesting in
`moq play` whenever a run fails while the path is still served, which loops
tightly on a broadcast that fails every time.

Fix it in the announce cursor or the follower so the gap surfaces, matching
JS. Regression with simulated time: an exact route ends, its request ends,
then a same-epoch prefix route takes over while a reader is still draining;
the follower must report the handoff, and `moq play` must restart. Leave the
follower unpolled until the replacement is announced, since awaiting its
`End` first avoids the bug and passes on the broken code, as
`follow_keeps_a_gap_between_routes_of_one_epoch` does today.

Once `follow` reports the gap itself, delete `moq_mux::container::ts::Follower`'s
same-epoch `Update` handling (#5147, maintainer 2026-10-10); its test,
`a_follower_continues_through_a_same_epoch_handoff_after_a_gap`, keeps its expectation.

Public API: no new items, but `follow` reports a gap where it reported an
`Update`, a behavior change to call out in the PR. Wire: none.
