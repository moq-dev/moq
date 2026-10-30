# [M] The queue host keeps the queue's disk

## Goal

Agent sessions sharing the self-hosted queue host can never fill its disk to
the point where the `moq-queue` runner fails a check, and a runner that does
crash comes back on its own.

## Plan

On 2026-10-30 the host filled twice (about 03:00 and 07:30 PDT). Each time
non-CI usage grew by roughly 480 GB and was later freed. CI's XFS quota is
whatever the host leaves free past a 32 GiB reserve, so the runner hit ENOSPC
mid-check: quest land ejected moq-dev/moq#6985 and moq-dev/moq#6999 as
`failed_checks`, and at 07:49 the runner aborted and stayed down for 36
minutes.

Done so far, in the host's configuration:
an `agent-storage-gc` user timer reclaims idle worktree build outputs and
leaked test scratch every 10 minutes and logs free space, and the runner unit
restarts after a crash (that change is in the PR but not yet deployed).

Remaining:

- Find what consumed the 480 GB, using the timer's free-space log the next
  time it happens, and fix that source.
- Guarantee CI a floor: put agent storage (`/home`, `/tmp`) in its own XFS
  project with a hard cap, so an agent gets ENOSPC instead of the queue.

Decided 2026-10-30 (maintainer, /quest-plan of the 2026-10-30 /quest-complete follow-ups): ✅ new m0 quest / note in the host configuration only / drop.

Public API: none. Wire: none.
