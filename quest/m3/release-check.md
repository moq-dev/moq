# [XS] Require Kixel Quest's release check on cut PRs

## Goal

The `release` branch's ruleset requires Kixel Quest's release check, which fails a PR into `release` while an investigation of a regression found since the last cut is still open, so a cut never ships an unexamined regression.
No PR into `main` is gated by it.

## Plan

Decided 2026-11-22 (maintainer, `/quest-plan` of the follow-ups of [kixel-ai/kixel.quest#91](https://github.com/kixel-ai/kixel.quest/pull/91)): ✅ release gates, never merge gates (rejected: milestone placement alone, which never stops a cut).

- Every PR into `release` is a cut or a backport ([RELEASE.md](/RELEASE.md)), so the check needs no label. As shipped, Kixel Quest learns which branches release PRs come from through a `## Releases` list in `RISK.md` and reports `kixel.quest release` on every open PR's head (its [README](https://github.com/kixel-ai/kixel.quest#readme)), so match that to this repository's cut branches. The check passes every other head and queue candidate at once.
- An investigation clears by landing, whether it fixed the regression or accepted it with a reason.
- m3, beside the release work it guards. The [cut](/quest/m3/release/cut.md) doesn't require it, so a delay in Kixel Quest never holds a release.

Public API: none. Wire: none.

