# [S] Benchmark sessions can't overlap on a host

## Goal

Two benchmark or profile sessions never run at once on one host. A
host-level lock, taken by `quest gate` or by moq's bench recipes (for example
`just bench-profile`, the relay CPU bisect driver, and `bench/run.sh`), makes
a second session wait, or refuse with a message naming the holder, instead of
measuring under the first one's load.

## Plan

Decided 2026-11-03 (maintainer, follow-up planning after the 2026-11-03
/quest-afk and /quest-complete runs), milestone:

- m2
- ✅ m0
- drop

Evidence: on 2026-11-03 `nixos2` had three rind agents and
[moq-dev/moq#7605](https://github.com/moq-dev/moq/pull/7605)'s relay-CPU
bisect running while [moq-dev/moq#7742](https://github.com/moq-dev/moq/pull/7742)'s
lock profile ran, which voided two of its three runs. Instruction counts are
load-sensitive too, so overlapping sessions skew even the metrics that avoid
wall-clock time.

Decide in this quest's PR whether the lock lives in moq or needs a
moq-dev/quest change, and record the choice and why here:

- **In moq.** The bench recipes take one host-wide lock, for example
  `flock` on a fixed path outside the checkout, so every worktree and agent
  shares it. That serializes benchmark sessions with each other, but builds
  and tests that other agents run through `quest gate` still start, since
  the gate only waits on load average and free memory.
- **In quest.** `quest gate` gains an exclusive admission that a bench
  takes, holding back other gated commands while it runs. Today the gate
  has no exclusive mode and no configuration (`quest gate --help`). That
  change goes into moq-dev/quest first, then moq adopts it with a pin bump,
  and this quest waits on the release.

Either way:

- Cover every entry point that measures: `bench/run.sh` (and the `just bench*`
  recipes that call it), `just bench-profile`, and the relay CPU bisect
  driver once #7605 lands it.
- The holder is visible (command, PID, start time), and a stale lock from a
  dead process is released.
- Nightly's hosted runners run one job per machine, so they need no change.

Public API: none. Wire: none.

## Related

- [Find and fix the relay's per-frame CPU growth since #5926](/quest/m1/release-perf/relay-cpu-bisect.md) - the bisect that overlapped
- [Log lock profile](/quest/m1/unbounded-groups/log-lock-profile.md) - the profile whose runs were voided
