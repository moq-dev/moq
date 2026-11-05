# [M] A replacement that fails before delivering never ends a live track

## Goal

When a route change splices a replacement source under a front, and the
replacement answers with metadata and then aborts before delivering any
group, readers keep receiving from the source that was serving before. The
track ends only when no source is left to serve it.

Not in scope: the producer-replacement redesign
([Resume as producer replacement](/quest/m2/resume-redesign.md)), which
replaces this machinery later.

## Plan

Decided 2026-11-05 (maintainer, follow-up from
[moq-dev/moq#7789](https://github.com/moq-dev/moq/pull/7789)): ✅ its own m1
quest, fixed now. Not chosen: leaving it to the m2 redesign.

#7789 fixed the case where the replacement aborts before the front reads its
answer, which now counts as a refusal. The remaining race is an abort after
the front's poll. `Front::track_info` (`rs/moq-net/src/model/front.rs`)
clears `draining` on splice, so the later `track_ended` refusal has no
incumbent to return to, and the track ends with the replacement's error.
Main has it too. Recommended fix: keep the incumbent's copy until the
replacement delivers its first group, then release it. Reproduce it first
with a test beside #7789's
`a_replacement_aborted_without_history_keeps_the_incumbent`
(`rs/moq-net/src/model/origin.rs`), and check whether `js/net` has the same
race.
