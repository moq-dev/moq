# [S] Latest readers join live by their Min Age

## Goal

The readers that wait for the largest group and raise their floor to
start near it (moq-audio, moq-video, and `@moq/room` chat)
join live by their Min Age instead (`Delay::min`, with no start anchor), with
no change in what they play. A reader that looks back states a larger Min Age,
such as moq-video's warmup lane, which keeps its one-group lookback so the
preceding intra-refresh group still decodes.
`moq play` drops its floor entirely and starts where its consumers' `Delay`
puts them.

## Plan

Changed 2026-11-20 (maintainer, /quest-plan of a pre-cut audit of the lite-07
and hang-04 wire changes): lite-07 has no relative anchors, and a live join is
no start anchor plus a stated Min Age, filled from upstream when a copy can't
prove its start ([moq-dev/moq#9265](https://github.com/moq-dev/moq/pull/9265)).
This quest's relative ranges became that join. `Range::live()`'s callers move
with the anchor's removal; these readers never used it.

Decided 2026-11-01 (maintainer, follow-up planning after the 2026-11-01
/quest-afk run): `moq play` drops its one-group `lookback` range floor and
relies on the `Delay` start instead. Since
[moq-dev/moq#7311](https://github.com/moq-dev/moq/pull/7311), its `open`
(`rs/moq-cli/src/play/media.rs`) passes each decoder's consumer a `Delay`:
the live decoder `{ min: video_delay, max: max_delay }` and the refresh
decoder `{ min + warmup, max + warmup }`, as `moq_video::decode::Consumer`
does. That start already decides where each decoder reads from, so the
`largest().await` plus `with_range(largest.group.saturating_sub(lookback)..)`
floor (and its copy in `play/video.rs`'s test helper `join`) is a second
start rule; #7311 kept it only to bound how long the refresh decoder's
start reach waits. Delete it rather than convert it to a live join.
Check that the reason in its comment still holds without it (a warm relay
copy waiting on its source must not replay its backlog), and that the
refresh decoder still starts on the group below the live edge.

Inventory with `rg 'largest\(|poll_largest' rs/ js/` first, since Rust chat
polls rather than awaits; keep any caller that needs the position itself
rather than a start. Each reader keeps its existing tests,
plus one against a warm relay copy.

Public API: none expected. Wire: none.
