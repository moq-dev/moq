# [XS] Tighten the AAC browser budgets after the reorder fix

## Goal

Seven scheduled nightlies on `main` after
[#5489](https://github.com/moq-dev/moq/pull/5489)
landed show the AAC `bursty` rows' `stalled_share`, skip-aheads, and discarded
audio at the level the Opus `bursty` rows show, on both rings, and
`test/audio-quality/budgets.json` carries ceilings re-measured from them.

## Plan

Waits on the nightlies: check the `audio-quality-run` artifacts of the
scheduled Nightly runs on `main` (`gh run download <id> -n audio-quality-run`)
once seven have run since the fix merged. Never dispatch the nightly by hand.

The fix measured reordered frames, which most AAC arrivals are: the publisher
flushes seven one-frame groups at once and the relay sends the newest first.
Replaying the 2026-10-13 nightly captures through it cut AAC bursty
`stalled_share` from about 0.30 to 0.08, skip-aheads from about 19 to 4 a
minute, and discarded audio from about 2500 to 500 ms a minute, and AAC wide
`stalled_share` from up to 0.10 to 0.005. Those captures were recorded while
the observer subscribed at the target, so they miss the groups that arrived
later than it, and every remaining replay stall follows one of those holes.
The fix also had the observer subscribe with the decoder's 2 s budget, so the
next nightlies capture every group the player sees.

Re-measure every AAC browser row by the rule in the budgets note, including the
AAC bursty `target_ms_p95` ceilings that were set from the replay, and replace
the `bursty-aac` replay trace with a capture from a post-fix nightly so it has
no holes. If the AAC bursty rows still stall well above Opus, the remaining
cause is new: reproduce it from the post-fix capture before changing anything.

## Related

- [Watch](/quest/m1/audio-jitter-target/watch.md) - the manual browser proof, which no longer waits on this
