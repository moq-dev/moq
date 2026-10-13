# [S] Audio quality budgets come from the nightly runner

## Goal

The nightly `Audio quality` job passes on `main`'s current player, and a breach
means a regression again. The browser rows' ceilings in
`test/audio-quality/budgets.json` are measured on the nightly runner, not on a
developer machine, and the file's `note` says how.

Non-goals: improving playout to meet tighter ceilings. That stays with
[Watch](/quest/m1/audio-jitter-target/watch.md). The replay rows, which PRs
already enforce, keep their own budgets.

## Plan

Seen 2026-10-02 to 2026-10-12: the job has failed every night since #4395
added it. The 2026-10-12 run (38276133321) breached 27 keys across the
Chromium rows, for example `chromium-aac-44100-bursty-isolated` at a
`stalled_share` of 0.324 against 0.01, and `target_ms_p95` on most Opus
rows. The browser ceilings were recorded locally (#4426), but nightly
measures on an `ubuntu-24.04-arm` runner. A red job nobody can act on hides
real regressions, so this moved out of Watch into m0 (maintainer,
2026-10-13).

- Measure across several scheduled nightly runs before setting ceilings;
  Nightly is never dispatched from a branch. A single sample's noise would make
  the job flaky. Failing runs keep their summaries as the
  `audio-quality-run` artifact for 7 days.
- Set each ceiling from the observed spread with modest headroom, and record
  the method in `budgets.json`'s `note`. Each loosened ceiling needs its
  reason in review (see `test/audio-quality/README.md`).
- Some rows may be genuine defects rather than runner noise. The AAC bursty
  rows stalling a third of the time look like one. Budget what the player
  does today, and file each suspected defect as a follow-up quest, so the job
  goes green without hiding it.
- Every row stays budgeted (decided 2026-10-13): a noisy row gets a wider
  ceiling from its measured spread. The harness has no per-row opt-out, and
  none is added.

Public API: none. Wire: none.

## Related

- [Watch](/quest/m1/audio-jitter-target/watch.md) - tightens these ceilings once playout improves
- [Audio quality native](/quest/m2/audio-quality-native.md) - reuses the same budget file for a native lane
