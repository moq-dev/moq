# [XS] TS open-GOP recovery tests cover a join at an in-flight group

## Goal

The H.264 open-GOP and HEVC RASL recovery tests also cover a TS export
that starts at a group already in flight, so a regression in how a join
orphans that group's leading pictures fails the merge gate.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7365](https://github.com/moq-dev/moq/pull/7365).
Rescoped 2026-11-17 on [moq-dev/moq#8830](https://github.com/moq-dev/moq/pull/8830), which deleted the MPEG-2 picture parser
and the MPEG-2 recovery arm: MPEG-2 video now goes out whole, so the
original goal of trimming zero-stuffed MPEG-2 leading B-pictures is dropped,
and that arm was the only one with a nonzero `join`.

In `rs/moq-mux/src/container/ts/recovery_test.rs`, give
`open_gop_trims_only_the_orphans` (`Profile::Open`) and
`hevc_trims_only_orphaned_rasl` (`Profile::Rasl`) an arm with a nonzero
`join` that lands inside a group, and assert what the export does with the
leading pictures of the group it starts on. Check the grader's join-group
accounting (`joined` in `recover`) models that case before relying on it.

Public API: none. Wire: none.
