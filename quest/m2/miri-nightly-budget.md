# [S] Nightly's Miri job stays inside its time budget

## Goal

Nightly's `Tests (rs miri)` entry finishes well inside the job's 60-minute
timeout on the 4-core ARM runner, and no single Miri test comes near
nextest's 15-minute `miri` profile cap, with headroom recorded from a real
Nightly run.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31 /quest-complete run), from [moq-dev/moq#7315](https://github.com/moq-dev/moq/pull/7315)
(merged), which finished the Miri for in-memory unsafe quest.
Milestone m2 (user: "use recommended for any CI/flakes").

Act on the first Nightly runs' timings, not ahead of them. Locally (24
cores) `just rs miri` took 11.5 min wall: strict pass 118 tests in 109 s,
epoch pass 94 in 307 s.

- No Nightly timing exists yet: the first run with the job
  ([run 41483654426](https://github.com/moq-dev/moq/actions/runs/41483654426),
  2026-11-02) failed in 2 minutes before any test, as cargo-miri found no
  `rust-src` ("given Rust source directory ... does not exist"). Install it
  with the toolchain (`components: miri` in `.github/workflows/nightly.yml`
  becomes `miri, rust-src`), then read the timings from the next run.
- `a_group_has_no_frame_cap` (`rs/moq-net/src/model/group.rs:5228` as of
  be39188000) took 307 s alone, writing 10,000 one-byte frames. Under `cfg(miri)`
  write fewer, still more than any former per-group frame cap, so the test
  keeps its meaning.
- `rs/moq-net/src/coding/reader.rs:1232` builds its payload byte by byte
  (`(0..SIZE).map(|i| (i % 251) as u8)`); switch to a cheaper fill if
  Miri starts running that module.
- If the job nears 60 minutes, split the strict and epoch passes into
  separate matrix entries (`.github/workflows/nightly.yml` Tests matrix,
  `timeout-minutes: 60` at line 328) rather than raising the timeout.

Public API: none. Wire: none.
