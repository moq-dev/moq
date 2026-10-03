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
