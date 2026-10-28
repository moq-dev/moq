# [S] Backport the frame-start refusal to release

## Goal

`@moq/net` on `release` refuses a lite group stream that starts at a nonzero
frame instead of silently numbering its frames from zero, and publishes a
patch release.

## Plan

Decided 2026-10-27 (maintainer): one PR against `release` per backport, in
m0, JS only. From
[moq-dev/moq#6922](https://github.com/moq-dev/moq/pull/6922), which makes
main honour a frame start. Release's Rust half does not apply (it lacks the
fill and group content model), and the #6903 and #6919 fixes are not
backported: #6919's `--stitch` is main-only, and #6903 would be a rewrite
against release's older watch API for a playback glitch.

As of 6ce67943f7, release's `js/net/src/lite/subscriber.ts` creates
`new netGroup.Producer(group.sequence)` and never reads the frame start, so
a mid-group start misnumbers frames, while release's Rust publisher can
send one. Refuse a nonzero start as main did before #6922 (the refusal
added in [moq-dev/moq#6395](https://github.com/moq-dev/moq/pull/6395)).

Public API: none. Wire: none; refuses a stream it misread before.
