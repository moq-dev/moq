# [XS] Gate cost can count instructions on the merge-queue runners

## Goal

Nightly's `Gate cost` job (`just gh gate-cost`) counts user-space instructions
with perf on the `nixos` merge-queue runners and passes.

## Plan

Found by the triage of Nightly run
[43447849289](https://github.com/moq-dev/moq/actions/runs/43447849289)
(2026-11-13). Gate cost first ran on `nixos-moq-ci` after
[moq-dev/moq#8515](https://github.com/moq-dev/moq/pull/8515) and failed with
`perf cannot count user-space instructions`. The sysctl in the hint is not
the cause: the runners are systemd-nspawn containers, and nspawn's default
seccomp filter denies `perf_event_open` (the host's
`kernel.perf_event_paranoid` is already -1).
[moq-dev/moq#8655](https://github.com/moq-dev/moq/pull/8655) corrects the
hint; the fix is host config in kixel-ai/dotfiles
`nixos/modules/ci-runners.nix`.

Decided 2026-11-13 (maintainer, /quest-plan):

- Containers: ✅ only the `moq-queue` ones get
  `--system-call-filter=perf_event_open` in `extraFlags` (rejected: all
  runners, which would let PR-branch code open perf events).
- Host: ✅ set `kernel.perf_event_paranoid=2` in the same change (rejected:
  keep -1, which lets those containers see CPU-wide and kernel events).
  Profiling on `nixos` then needs sudo.

Open the dotfiles PR, noting that open dotfiles PR #125 also edits that file.
Check with `perf stat -e instructions:u true` as the runner user inside a
queue container, then the next scheduled Nightly's Gate cost job; never
dispatch Nightly.

## Waiting

- Maintainer: merge the dotfiles PR and rebuild `nixos`, which restarts the CI runner containers; an agent does not deploy host config. Check with the `perf stat` command above.
