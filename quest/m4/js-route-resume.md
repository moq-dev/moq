# [L] A JS route switch resumes the newest group past the frames held

## Goal

A JS reader (`js/net/src/resume.ts`) whose route moves to a new copy asks that copy for the newest group it handed out from the frame after those a copy holds, as Rust does since [moq-dev/moq#9155](https://github.com/moq-dev/moq/pull/9155), so a route moving behind one next hop sends none of that group again over a lite-07 link.
The new copy's cache still holds the group whole for a reader that joins after the old copy is gone.

## Plan

- Rust's shape to mirror: `Reader::held_at` and `resume_at` pick the position, `Producer::subscription` returns a `Resumed` that lends the head with the demand, `Subscription::resume` aggregates only when every reader resumes, and the copy's track keeps one loan per group (`TrackState::lent`), bounded by `group::MAX_CACHE_BYTES`, which the group takes as it arrives partway through.
- JS has no cross-copy head adoption in its cache today, so the loan needs that first: a group arriving partway through takes the frames lent for it before any reader sees it.
- The wire already works in JS: the publisher serves a Position start (`positionFrames`), and `encodeSpan` writes one when a span's `startFrame` is set; only the reader side never asks for one.
- Port Rust's tests: `a_switch_resumes_the_newest_group_past_the_frames_held`, `a_resumed_copy_holds_the_group_whole_for_a_later_reader`, `a_query_resuming_a_new_copy_lends_it_the_head`, and the loan tests in `model/track.rs`.
- Today a JS relay resends that one group, which costs bandwidth but loses nothing, so this waits behind the Rust relay work.
