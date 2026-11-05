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

Decided 2026-11-03 (maintainer, /quest-plan of the follow-ups from [moq-dev/moq#7667](https://github.com/moq-dev/moq/pull/7667)):
moved from m2 under [Nightly passes on main](/quest/m0/nightly/README.md),
since the missing `rust-src` keeps the m0 Nightly red. The scheduled run of
2026-11-03 ([run 41656613559](https://github.com/moq-dev/moq/actions/runs/41656613559/job/125988511062))
still fails the same way.

The setup fix in [moq-dev/moq#7812](https://github.com/moq-dev/moq/pull/7812)
installs `miri, rust-src` together. The scheduled failure above is before
any test, so there is still no scheduled ARM timing evidence. Keep this
quest until a scheduled run containing that fix records the job duration,
strict and epoch pass durations, slowest test, and headroom against both
budgets. Do not dispatch Nightly by hand to obtain it.

In the same log, confirm moq-net's `model::frame::` tests ran (the strict
pass selects them in `sh/rs/miri.sh`), including the lock-free single-delivery
path [moq-dev/moq#7508](https://github.com/moq-dev/moq/pull/7508) added; extend
the selection if any are missed. (Folded in from moq-dev/moq#7748's
`miri-frame.md`, 2026-11-04, whose `rust-src` half #7812 landed.)

Act on those timings before optimizing. Earlier local measurements on 24
cores took 11.5 min wall: strict pass 118 tests in 109 s, epoch pass 94 in
307 s. These are desktop measurements, not proof of the 4-core ARM budget.

- `a_group_has_no_frame_cap` (`rs/moq-net/src/model/group.rs`) took 307 s
  alone, writing 10,000 one-byte frames. If scheduled timings warrant a
  smaller Miri workload, it must still exceed the former 8192-frame cap.
- `rs/moq-net/src/coding/reader.rs` builds its payload byte by byte
  (`(0..SIZE).map(|i| (i % 251) as u8)`); consider a cheaper fill only if
  Miri starts running that module and measurements justify it.
- If the job nears 60 minutes, split the strict and epoch passes into
  separate matrix entries rather than raising the timeout. Preserve
  nextest's 15-minute per-test cap.

Public API: none. Wire: none.

## Waiting

- moq-dev/moq: After the setup fix lands, the next scheduled Nightly on
  `main` must run `Tests (rs miri)` on `ubuntu-24.04-arm`. Read its log and
  record the timings and headroom above; local runs do not clear this wait.
