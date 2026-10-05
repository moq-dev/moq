# [S] Subscription cut by publisher disconnect

## Goal

moq-tokio
`subscription_end_integrity::a_subscription_cut_by_the_publisher_disconnecting_does_not_end_clean`
passes in every full-suite run.

## Plan

It ends `Ok(None)` with 10 of 20 frames in 3 of 8 full-suite runs on a clean
tree, and passes alone (#4332). A clean end after a publisher disconnect is a
real bug if the code can produce it, not only a test race: reproduce under
load first, find which path reports the cut as a clean end, and fix it there.

Recorded in the 2026-10-05 audit: [#4662](https://github.com/moq-dev/moq/pull/4662)
(closed 2026-10-01) did not reproduce it: eight full `moq-tokio` suite runs
passed every test, including this one. #4533 had already hardened
active-stream tail settling, and the aborted groups arrival readers skip are
[SUBSCRIBE_DROP](/quest/m1/subscribe-drop.md)'s, which overlaps this. Recheck
under the original broader workspace load, or after SUBSCRIBE_DROP lands,
before fixing anything.
