# [S] Test-harness runs live under the cargo target directory

## Goal

A failed test-harness run (interop, smoke, and the other `test/lib/harness.sh`
callers) is kept under the cargo target directory, so its staged binaries are
hard links again and a kept run goes away with the worktree, instead of copying
about 4.3 GB of binaries into TMPDIR. `HARNESS_RUNS` still moves the root. How
interop runs are scheduled is out of scope.

## Plan

Found 2026-11-14 by [kixel-ai/dotfiles#164](https://github.com/kixel-ai/dotfiles/pull/164),
which made each agent session's TMPDIR a 12 GiB tmpfs on the NixOS host: fedora's
`/tmp/moq-test-1000` held 101 to 110 GB of kept runs. `harness_begin` roots runs
in `${TMPDIR:-/tmp}/moq-test-$UID`, and `harness_stage` hard-links each binary
into the run, falling back to a copy when TMPDIR is on another filesystem than
the target directory. A failing run keeps everything.

Decided 2026-11-14 (maintainer, /quest-plan): ✅ stage and keep each run
directory under the target directory, so staging hard-links on the same
filesystem and a kept failed run, binaries included, is deleted with the
worktree (rejected: keep run directories in TMPDIR but delete staged binaries
on failure, keeping only logs).

Guidance:

- Resolve the root from `cargo metadata`'s `target_directory`, as
  `test/interop/interop.sh` already does for `TARGET_BASE`, so a custom
  `CARGO_TARGET_DIR` still shares a filesystem with the binaries. Port claims
  stay in `/tmp`, which is shared across worktrees on purpose.
- AGENTS.md says scratch never goes in `target/`, because build output may be
  wiped. A kept run is disposable evidence, so narrow that line in the same PR
  rather than contradict it; say in the PR how a wipe during a live run behaves
  (a hard-linked binary survives its source being unlinked, not its run
  directory being removed).
- Update `test/README.md`, which documents the root.

Fixes [moq-dev/moq#7715](https://github.com/moq-dev/moq/pull/7715), which began
staging binaries into run directories with a copy fallback, under the TMPDIR
root that [moq-dev/moq#3509](https://github.com/moq-dev/moq/pull/3509) chose.

Public API: none. Wire: none.
