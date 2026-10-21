# [XS] Three flakes and a fresh-worktree interop

## Goal

Each of these passes reliably under a loaded `just check`, fixed at its
cause:

- OBS "stop drains the session" (`moq-output-test.cpp`, a 16 MiB frame under
  TSan), seen in [moq-dev/moq#6131](https://github.com/moq-dev/moq/pull/6131)
  and [moq-dev/moq#6137](https://github.com/moq-dev/moq/pull/6137).
- The js/net RSS memory test, which flakes under load
  ([moq-dev/moq#6128](https://github.com/moq-dev/moq/pull/6128)).
- The audio-quality harness's RTT sample: since
  [moq-dev/moq#5534](https://github.com/moq-dev/moq/pull/5534), `probe.ts`
  peeks a probe an audio-only page never observes, so samples lack `rtt`
  (#6128).

And `just test interop` runs in a fresh worktree instead of failing on
missing bun dependencies (#6137).

## Plan

Decided 2026-10-21 with the maintainer: one quest for the four, since each
is small. Reproduce each before fixing, and follow the epic's rules: no
retries, sleeps, or raised timeouts.

- The RSS test should measure the allocation in a way that does not depend
  on process RSS.
- The RTT probe should subscribe while sampling, as
  `js/watch/src/ui/stats.ts` does.
- Interop should install what it needs, as the other `just` recipes do,
  rather than assume an earlier `bun install`.

Public API: none. Wire: none.
