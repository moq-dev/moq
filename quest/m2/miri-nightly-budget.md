# [S] Nightly's Miri job stays inside its time budget

## Goal

Nightly's `Tests (rs miri)` entry finishes well inside the job's 60-minute
timeout on the 4-core ARM runner, and no single Miri test comes near
nextest's 15-minute `miri` profile cap, with headroom recorded from a real
Nightly run.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7315](https://github.com/moq-dev/moq/pull/7315),
which finishes [Miri for in-memory unsafe](/quest/m2/miri-memory.md).
Milestone m2 (user: "use recommended for any CI/flakes").

Act on the first Nightly runs' timings, not ahead of them. Locally (24
cores) `just rs miri` took 11.5 min wall: strict pass 118 tests in 109 s,
epoch pass 94 in 307 s.

- `a_group_has_no_frame_cap` (`rs/moq-net/src/model/group.rs:5111`)
  took 307 s alone, writing 10,000 one-byte frames. Under `cfg(miri)`
  write fewer, still more than any former per-group frame cap, so the test
  keeps its meaning.
- `rs/moq-net/src/coding/reader.rs:1046` builds its payload byte by byte
  (`(0..SIZE).map(|i| (i % 251) as u8)`); switch to a cheaper fill if
  Miri starts running that module.
- If the job nears 60 minutes, split the strict and epoch passes into
  separate matrix entries (`.github/workflows/nightly.yml` Tests matrix,
  `timeout-minutes: 60` at line 347) rather than raising the timeout.

Public API: none. Wire: none.

## Required

- [In-memory unsafe runs under Miri](/quest/m2/miri-memory.md) - its PR, [moq-dev/moq#7315](https://github.com/moq-dev/moq/pull/7315), adds the Miri job and nextest profile
