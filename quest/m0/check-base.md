# [XS] just check diffs against the PR base

## Goal

`just check` and `just fix` diff against the branch's real base (the PR's
base, `origin/main` by default), whatever the local branch is called and
whatever it tracks, so a scoped check never turns into a workspace-wide run.

## Plan

Found in #5141 (2026-10-10): an agent iterating a PR on a local branch named
differently from the PR branch ran `just check`, which diffed against
`origin/quest/m1/held-group-wakes` and started a workspace-wide test on the
shared machine. `sh/changed.sh` takes `@{upstream}` as the base and only falls
back to `origin/main` when the upstream has the local branch's own name, so a
branch tracking any other branch (for example a PR head under another local
name) is diffed against that branch.

Decided 2026-10-10, in m0 because every agent's check on this shared machine
pays for it: pick the base from what the branch merges into, not from what it
tracks. For example, prefer the open PR's base when `gh` finds one, then an
upstream named `main` or `release`, then `origin/main`. `GITHUB_BASE_REF` in CI
and an explicit `BASE` argument keep winning. Print the chosen base, as today.
A stacked PR diffs against its base PR's branch. A missing, offline, or
unauthenticated `gh` falls through quickly and silently, so a local check
never hangs or fails on the lookup.
If the change contradicts AGENTS.md's "set the upstream to the base branch",
fix that line in the same PR.

Public API: none. Wire: none.
