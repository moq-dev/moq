# [S] Pin Quest's expensive-command gate

## Goal

The pinned Quest CLI provides `quest gate` so local entrypoints can share CPU
admission with other repositories. This quest stays ready while awaiting the
upstream implementation.

## Plan

- Condition: the [Quest gate](https://github.com/kixelated/quest/blob/b87c4c0ed44d4c4b6ae2e344bfa9387004d11237/quest/m0/local-gate.md)
  has landed upstream. Check its availability and advance the flake input to
  a reachable upstream revision in a focused pin PR.
- Verify the pinned binary's help and small command/exit-status smoke test in
  the Nix devshell. Keep adoption of repository entrypoints in its own quest.
- Decided 2026-10-15: m0, independent of worktree lifecycle and test rewrites;
  the CPU gate should not wait for unrelated Quest features.
