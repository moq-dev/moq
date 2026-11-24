# [XS] Require Kixel Quest's release check on cut PRs

## Goal

The `release` branch's ruleset requires Kixel Quest's release check, which fails a cut PR while an investigation of a regression found since the last cut is still open, so a cut never ships an unexamined regression.
A backport passes: it ships a serious fix, which no unrelated regression on `main` should hold.
No PR into `main` is gated by it.

## Plan

Decided 2026-11-22 (maintainer, `/quest-plan` of the follow-ups of [kixel-ai/kixel.quest#91](https://github.com/kixel-ai/kixel.quest/pull/91)): ✅ release gates, never merge gates (rejected: milestone placement alone, which never stops a cut).

- Kixel Quest tells a release PR by its head branch, from a `## Releases` list in `RISK.md` that this quest adds, and reports `kixel.quest release` on every open PR's head (its [README](https://github.com/kixel-ai/kixel.quest#readme)). A cut's head is `main` ([RELEASE.md](/RELEASE.md)), so the list holds `main`; a backport's head is any other branch, so it passes, as does every other head and queue candidate.
- An investigation clears by landing, whether it fixed the regression or accepted it with a reason.
- m3, beside the release work it guards. The [cut](/quest/m3/release/cut.md) doesn't require it, so a delay in Kixel Quest never holds a release.

Public API: none. Wire: none.

