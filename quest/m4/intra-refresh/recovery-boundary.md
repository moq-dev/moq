# [S] Plan: recovery boundaries remain safe as refresh duration changes

## Goal

Settle how consumers know a group's precise recovery boundary before
presenting pictures, including when later refresh sweeps take longer than
earlier ones. Rewrite the import and consumer quests around that contract.

## Plan

Decided 2026-11-08 (maintainer, /quest-plan): intra refresh is not part of
this cut, so `warmup` ships with its current duration contract and this plan
settles only what the importer needs.

Moved to m4 2026-11-12 (maintainer, /quest-audit): no downstream application
waits on intra-refresh import, and no native publisher sets video `warmup`.
The boundary stays a catalog field, never a LOC frame property or other
per-frame container field, and it is a duration, not a frame count. A
per-group exact boundary carried in the media was offered and declined.
Re-interview the maintainer before choosing how a catalog duration stays
safe when a later sweep is longer, since a catalog update and the groups
it covers travel on separate tracks with no ordering between them.

The 2026-10-11 TS audit found the proposed catalog duration is not yet a
sufficient guarantee. `h264-import` raises the global value after observing
a longer sweep.
`consumer-warmup` sizes the transcoder's decode history from that duration. A
later catalog update cannot
recall pictures a subscriber has already displayed using the old value.

Decided 2026-10-22 (maintainer): precision is settled; the hang draft rounds a
fractional-millisecond `warmup` up
([#5212](https://github.com/moq-dev/moq/pull/5212)). The draft's `warmup` rule
is amended so a consumer running a parallel max-delay decoder MAY present
min-delay pictures before the group start plus `warmup`, which keeps the
merged dual decoder in `decode::Consumer`
([#5842](https://github.com/moq-dev/moq/pull/5842)), and the draft now says
so. Publication ordering for a later, longer sweep remains to settle.

Ask the maintainer how the catalog duration stays safe for a longer sweep,
such as a bound published before the affected content or a fixed bound from
config, including the latency each costs. Test timestamp precision, reordered
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

## Waiting

- Maintainer: how a catalog `warmup` stays safe when a later sweep is longer than the published one. Options: a bound published before the content it covers (costs one sweep of latency up front); a fixed upper bound from importer config; refusing the stream once a sweep exceeds the published `warmup`. Recommended: refuse, matching h264-import's rule that a stream is refused rather than published with a guess, and let a configured bound raise the limit where an operator knows its encoder.

## Related

- [H.264 import](/quest/m4/intra-refresh/h264-import.md) - publishes recovery information
- [Consumer warmup](/quest/m4/intra-refresh/consumer-warmup.md) - uses it to size the transcoder's decode history
