# [XS] Parallelize shellcheck in sh/shell.sh

## Goal

`sh/shell.sh check` runs `shellcheck` in parallel across CPU cores so linting
tracked shell scripts takes a few seconds instead of ~25s serially.

## Plan

`sh/shell.sh` currently invokes a single serial `shellcheck "${scripts[@]}"`
over every tracked shell script (~24.7s wall time).

- Chunk and parallelize `shellcheck` across available CPUs (for example via
  `printf '%s\0' "${scripts[@]}" | xargs -0 -n 16 -P "$(getconf _NPROCESSORS_ONLN)" shellcheck --`),
  preserving exact exit-code failure on any chunk error and avoiding races on
  untracked files.
- Measure `sh/shell.sh check` wall time before and after and verify `just gh
  check` passes.

Decided 2026-11-21 (afk, `/quest-plan`):

- Placement: ✅ `quest/m0/ci/parallel-shellcheck.md` (belongs in the active m0
  CI cost and latency epic; rejected: top-level `m0`, `m1`, `m3`).
