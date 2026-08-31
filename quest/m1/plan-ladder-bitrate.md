# [S] Plan: demand-driven ladder bitrate

## Goal

A settled, per-PR split of [#2858](https://github.com/moq-dev/moq/issues/2858)
exists in the quest tree. Today the quest cannot land as one PR: the umbrella stacks several phases on dev's bandwidth allocator and partially duplicates #2848 and #2859.

## Plan

Run /plan-quest with the maintainer: cut the phases into per-PR quests and reconcile the overlap with the sibling allocator quests. The resulting quests replace
[the parent quest](/quest/m1/2858-transcode-ladders-encode-every-live-rung-at-full-bitrate.md) and carry its Closes entries forward.
