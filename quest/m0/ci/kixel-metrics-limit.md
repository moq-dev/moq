# [XS] Benchmark reports fit Kixel Quest's 8 MiB metrics budget

## Goal

`sh/gh/bench-report.py` checks each `kixel-metrics` `metrics.json` against
Kixel Quest's new 8 MiB inflated-file budget instead of 1 MiB, and
`bench/README.md` states the same figure. That leaves Nightly and CodSpeed
room to grow before their upload steps fail.

## Plan

Decided 2026-11-23 (maintainer, /quest-run): ✅ plan it (rejected: drop).
Kixel Quest's single 1 MiB `LIMIT` is being split. Artifact zips stay at
1 MiB compressed, and each inflated file gets 8 MiB
([kixel-ai/kixel.quest#118](https://github.com/kixel-ai/kixel.quest/pull/118)).
A full Nightly `metrics.json` is 865 KB today, and CodSpeed's is 161 KB.

- Raise `KIXEL_LIMIT` in `sh/gh/bench-report.py` to `8 << 20`, and the
  "at most 1 MiB" sentence in `bench/README.md`.
- Keep the step failing above the limit, so moq never uploads a file Kixel
  Quest would skip.
- Each artifact zip must still fit Kixel Quest's 1 MiB compressed budget. A
  full Nightly compresses to about 97 KB, so nothing needs to check that yet.

## Waiting

- kixel-ai/kixel.quest: the 8 MiB inflated budget is deployed. Check that
  `quest/m0/metrics-budget.md` is gone from kixel.quest's `main` and that
  `src/metrics.ts` no longer caps an inflated file at `1 << 20`.
