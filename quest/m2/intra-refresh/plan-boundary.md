# [S] Plan: recovery boundaries remain safe as refresh duration changes

## Goal

Settle how consumers know a group's precise recovery boundary before
presenting pictures, including when later refresh sweeps take longer than
earlier ones. Rewrite the import and consumer quests around that contract.

## Plan

The 2026-10-11 TS audit found the proposed catalog duration is not yet a
sufficient guarantee. `h264-import` rounds a measured recovery duration down
to milliseconds and raises the global value after observing a longer sweep.
`consumer-warmup` sizes the transcoder's decode history from that duration. A
rounded value can leave the history short of the recovery picture, and a later catalog update cannot
recall pictures a subscriber has already displayed using the old value.

Decided 2026-10-22 (maintainer): precision is settled; the hang draft rounds a
fractional-millisecond `warmup` up
([#5212](https://github.com/moq-dev/moq/pull/5212)). The draft's `warmup` rule
is amended so a consumer running a parallel max-delay decoder MAY present
min-delay pictures before the group start plus `warmup`, which keeps the
merged dual decoder in `decode::Consumer`
([#5842](https://github.com/moq-dev/moq/pull/5842)), and the draft now says
so. Publication ordering for a later, longer sweep remains to settle.

Ask the maintainer to choose between an exact per-group boundary and a safe
bound known and published before the affected content, including the latency
and catalog/container consequences. Test timestamp precision, reordered
pictures, frame_num wrap, variable sweep lengths, a missed recovery picture,
and a join/skip before a longer sweep is known. Decode-only pictures still
build reference state; withholding presentation is not dropping their input.

Review the H.264 recovery-point flags and count semantics and HEVC
CRA/RADL/RASL separately. A timestamp-before-keyframe rule is a conservative
leading-picture policy, not a complete codec dependency model; the TS export
already keeps HEVC RADL pictures, by NAL type, and
`rs/moq-mux/src/container/ts/recovery_test.rs` grades actual group skips and
recovered output for H.264 open GOP and HEVC RASL and RADL. State which
profiles it supports without changing an existing wire contract by inference.

Produce implementation and automated software-decode tests in the existing
quests. Use generated fixtures, run short cases in CI, and retain full
reference-decode comparisons for continuous playback and each recovery case.

## Related

- [H.264 import](/quest/m2/intra-refresh/h264-import.md) - publishes recovery information
- [Consumer warmup](/quest/m2/intra-refresh/consumer-warmup.md) - uses it to size the transcoder's decode history
