# [S] Test-harness runs live in the checkout's .tmp/

## Goal

A failed test-harness run (interop, smoke, and the other `test/lib/harness.sh`
callers) is kept under the checkout's `.tmp/` (such as `.tmp/harness/`), so a
kept run goes away with the worktree and never copies about 4.3 GB of staged
binaries into TMPDIR. `HARNESS_RUNS` still moves the root. How interop runs are
scheduled is out of scope.

## Plan

Found 2026-11-14 by [kixel-ai/dotfiles#164](https://github.com/kixel-ai/dotfiles/pull/164),
which made each agent session's TMPDIR a 12 GiB tmpfs on the NixOS host: fedora's
`/tmp/moq-test-1000` held 101 to 110 GB of kept runs. `harness_begin` roots runs
in `${TMPDIR:-/tmp}/moq-test-$UID`, and `harness_stage` hard-links each binary
into the run, falling back to a copy when the run is on another filesystem
than the binary. A failing run keeps everything.

Decided 2026-11-14 (maintainer, on [moq-dev/moq#8793](https://github.com/moq-dev/moq/pull/8793)):
✅ keep each run directory, staged binaries included, in the checkout's `.tmp/`
(such as `.tmp/harness/`), beside TLC's `.tmp/tlc/`, so AGENTS.md's "scratch
never goes in `target/`" stands unchanged (rejected: the target directory,
narrowing that rule; keeping runs in TMPDIR but deleting staged binaries on
failure, keeping only logs).

Hard links: mbx keeps build output outside the checkout, behind a `target`
link (`~/.cache/mbx/targets` on desktop, `/var/cache/mbx/targets` on fedora
and nixos). On 2026-11-14 each shared one XFS root filesystem with `~/work`,
so staging still hard-links on all three. Where they differ, `harness_stage`
falls back to `cp -L` into `.tmp/`: a full copy per run, but on the checkout's
disk and removed with the worktree, never in TMPDIR. `cp -L --reflink=auto`
makes that copy nearly free on btrfs or XFS; worth taking in the same change.

Guidance:

- Resolve the root from the checkout (`git rev-parse --show-toplevel`), since
  harnesses change directory after `harness_begin`. Port claims stay in
  `/tmp`, which is shared across worktrees on purpose.
- Update `test/README.md`, which documents the root.

Fixes [moq-dev/moq#7715](https://github.com/moq-dev/moq/pull/7715), which began
staging binaries into run directories with a copy fallback, under the TMPDIR
root that [moq-dev/moq#3509](https://github.com/moq-dev/moq/pull/3509) chose.

Public API: none. Wire: none.
