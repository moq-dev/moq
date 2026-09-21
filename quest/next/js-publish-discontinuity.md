# [S] js/publish declares a discontinuity with a marker group

## Goal

A browser publisher declares a discontinuity the same way Rust does: the
js/hang container producer publishes a marker group of one empty frame and
resumes in the next group, and js/publish calls that on encoder restart.

## Plan

js/hang has no container producer `discontinuity()` today (only comments in
`js/publish/src/audio/encoder.ts:154`). Add one that matches the hang marker
contract: cut the open group, write a single empty frame at the exclusive end
of the previous epoch, finish that group, and let the next keyframe open the
resume group. Wire encoder restart in js/publish to it. Data tracks skip a
sequence with no marker.

## Required

- [Merge dev](/quest/dev/merge-dev.md) - the required dev APIs must be available on main before this implementation starts
