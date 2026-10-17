# [L] TS recovery is tested against actual missing MoQ content

## Goal

Automated tests show how TS output and software decoding recover when MoQ
objects, groups, or tracks are absent or too late, including after playback
has started. Unaffected tracks continue according to the supported profile;
missing dependency chains are not presented as independently decodable.

## Plan

The cold-acquisition baseline is blocked by [#5713](https://github.com/moq-dev/moq/pull/5713).
Reproduced on `main` at `e6006793f` on 2026-10-16 with that PR's
`a_start_group_cut_short_starts_at_the_next` paused-clock regression, omitting
only the counters that the PR adds: after the first group is reset, export
still writes its five pictures at PTS 0 through 14400 before the next whole
group at PTS 54000. The oracle requires starting with that whole group.
Keep the production fix and its regression with the source quest below;
resume the broader recovery evidence after it merges.

Test content loss separately from IP packet loss. QUIC may retransmit IP
loss; a netem percentage does not establish that any MoQ object was omitted.
Keep the real network impairment arm for transport and pacing evidence, and
add deterministic fault schedules at the MoQ content boundary.

Cover a cold join, a skipped group, a partially delivered group, late objects
that miss release, one stalled/missing track, and resumed delivery. Put loss
around PAT/PMT or catalog acquisition, codec configuration and recovery
points, audio/video skew, PCR/PTS wrap and declared clock discontinuity.
Choose fixtures from declared supported profiles, not an unbounded matrix.

Use mocked time for the Rust scheduling regressions. Integration captures
record exactly which content was delivered, omitted or late, so the oracle
does not explain every output gap as acceptable loss. Decode surviving
content with FFmpeg, compare recovered frames/samples against a continuous
source decode where applicable, and assert recovery at the profile's valid
acquisition point. No arbitrary recovery timeout becomes a substitute for
the encoded dependency rule. Exercise the graders' failure paths in CI.

For semantic TS, validate continuity/discontinuity signalling, surviving
access-unit deadlines, decoder buffers and actual decoded output. Recovery
trims apply after a latency skip or discontinuity as well as the first join.
Coordinate the existing open-GOP and gradual-refresh quests: an H.264
recovery-point SEI does not always promise immediate recovery, and HEVC CRA,
RADL and RASL do not have identical reference rules. Continuous subscribers
must retain valid pictures.

Short deterministic arms run on relevant PRs. Long drift and impairment
arms run nightly with failure artifacts. Keep byte-position PCR accuracy
and measured release timing as separate checks.

## Required

- [TS decoded content](/quest/m1/test-flakes-2/ts-video-content.md) - a reset acquisition group is discarded before TS output begins

## Related

- [Automated validation](/test/ts/README.md) - shared graders, profiles and known-bad controls
- [Gradual recovery](/quest/m2/intra-refresh/README.md) - recovery counts and per-group warmup
