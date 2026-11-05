# [S] Media fuzz findings

## Goal

Every media fuzz target in `nightly.yml` (`rs/moq-mux/fuzz` and
`rs/moq-rtmp/fuzz`) runs a full nightly session without a crash or a
bounded-work violation, so the jobs find new bugs instead of the same ones.
Each fixed finding's minimized input is committed under the target's
`regressions/`, which the merge gate replays.

## Plan

[moq-dev/moq#7313](https://github.com/moq-dev/moq/pull/7313) added the
targets, and its children fixed the findings from its short local runs.
Each fix commits its input under `regressions/` with the fix, since an
input committed before the fix would fail the merge gate.

Decided 2026-11-05 (maintainer, /quest-plan): this epic stays an `[S]`
watcher. An agent triages each red target into a child quest; an
intermittent failure joins m0's [test flakes](/quest/m0/test-flakes-2/README.md)
instead. The epic is deleted once every target runs green. Not chosen:
deleting it once its last child merged.

Triage of the 2026-11-03 to 2026-11-05 scheduled runs
([moq-dev/moq#7961](https://github.com/moq-dev/moq/pull/7961)):

- `annexb` (scuffle-h265 SPS overflow) and `ts` (step look-ahead overflow),
  2026-11-03: fixed by [moq-dev/moq#7685](https://github.com/moq-dev/moq/pull/7685)
  and [moq-dev/moq#7683](https://github.com/moq-dev/moq/pull/7683), which
  committed the inputs.
- `cmaf` (out of memory on received metadata), 2026-11-04: fixed by
  [moq-dev/moq#7843](https://github.com/moq-dev/moq/pull/7843); moq-dev/moq#7961
  commits its input as `cmaf/metadata-preflight`.
- `fmp4` (allocation bound), 2026-11-04 and 2026-11-05: a field-less `trun`
  declared 65,792 samples in a 104-byte `moof`. Fixed in moq-dev/moq#7961 by
  holding a fragment to one sample per metadata byte past a base.

What remains: watch the next scheduled runs. When every media target has
run green, delete this epic. A new finding becomes a child here.

For the crates outside the `kixelated` and `moq-dev` orgs, moq-mux fixes
the bug on its own side and the bug is reported upstream (decided
2026-10-30, maintainer): [austinleroy/webm-iterable#11](https://github.com/austinleroy/webm-iterable/issues/11),
[therishidesai/h264-parser#9](https://github.com/therishidesai/h264-parser/issues/9),
[ScuffleCloud/scuffle#653](https://github.com/ScuffleCloud/scuffle/issues/653)
(scuffle-h265), and [ScuffleCloud/scuffle#654](https://github.com/ScuffleCloud/scuffle/issues/654)
(scuffle-expgolomb). Posting anything further on those repos still needs the
maintainer's approval.
