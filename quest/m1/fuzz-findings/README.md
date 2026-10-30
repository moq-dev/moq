# Media fuzz findings

## Goal

Every media fuzz target in `nightly.yml` (`rs/moq-mux/fuzz` and
`rs/moq-rtmp/fuzz`) runs a full nightly session without a crash or a
bounded-work violation, so the jobs find new bugs instead of the same ones.
Each fixed finding's minimized input is committed under the target's
`regressions/`, which the merge gate replays.

## Plan

[moq-dev/moq#7313](https://github.com/moq-dev/moq/pull/7313) added the
targets and landed with six nightly jobs red (decided 2026-10-30,
maintainer). Its short local runs found the bugs the children fix; the
minimized inputs are in [its PR comment](https://github.com/moq-dev/moq/pull/7313#issuecomment-6520535160).

Each child commits its input under `regressions/` with its fix, since an
input committed before the fix would fail the merge gate. After the last
child, run every target nightly for a few sessions; a new finding becomes a
child here.

For the crates outside the `kixelated` and `moq-dev` orgs, moq-mux fixes
the bug on its own side and the bug is reported upstream (decided
2026-10-30, maintainer): [austinleroy/webm-iterable#11](https://github.com/austinleroy/webm-iterable/issues/11),
[therishidesai/h264-parser#9](https://github.com/therishidesai/h264-parser/issues/9),
[ScuffleCloud/scuffle#653](https://github.com/ScuffleCloud/scuffle/issues/653)
(scuffle-h265), and [ScuffleCloud/scuffle#654](https://github.com/ScuffleCloud/scuffle/issues/654)
(scuffle-expgolomb). Posting anything further on those repos still needs the
maintainer's approval.

## Required

- [moq-mux on a fixed mp4-atom](/quest/m1/fuzz-findings/mp4-atom-bump.md) - waiting on a 0.17.x release containing kixelated/mp4-atom#264, then moq-mux moves off 0.16.2, which also picks up the `iref` fix
- [MKV block headers never panic](/quest/m1/fuzz-findings/mkv-block-header.md) - a short SimpleBlock or Block is an import error, not a webm-iterable index panic
- [TS step scan stays ordered](/quest/m1/fuzz-findings/ts-step-scan.md) - the step detector's look-ahead never subtracts past its start
- [SPS fields cannot overflow](/quest/m1/fuzz-findings/sps-overflow.md) - hostile H.264 and H.265 SPS crop and size fields are refused instead of wrapping
- [mp4-atom iloc is bounded](/quest/m1/fuzz-findings/mp4-atom-iloc.md) - zero-size `iloc` extents cannot allocate megabytes from a few bytes
- [A catalog update reuses its encoder](/quest/m1/fuzz-findings/catalog-encoder.md) - republishing a compressed catalog or HLS group stops allocating a fresh DEFLATE encoder (about 200 KB) each time

## Related

- [MKV import streams blocks](/quest/m1/security-audit/mkv-stream-blocks.md) - also reads the MKV block header beside ebml-iterable
- [fMP4 import streams mdat](/quest/m1/security-audit/fmp4-stream-mdat.md) - the same importers' memory bounds
