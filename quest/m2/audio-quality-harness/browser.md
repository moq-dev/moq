# [L] The browser audio lane is graded against a budget, nightly

## Goal

`just test audio-quality` plays a broadcast in a headless browser over a seeded,
impaired path and prints a graded table: underruns, short quanta, discarded
samples, skip-aheads, and where the end-to-end delay went. It fails when a
profile exceeds its budget. A nightly job runs the full matrix and keeps the
run directory of a failure.

The same run works on both ring paths, isolated and postMessage, since the
production path is the one without cross-origin isolation.

## Plan

Upstream the reporter's harness from `fperex/moq` branch `debug/rt-audio`
rather than rebuilding it, keeping the attribution. It already has the CDP
driver, the beacon sink, the analyzer, the ring replay, `bench.sh`'s five
scenarios, and `compare.mjs`. What it does not have is a home in `test/`, a
budget, or a schedule.

- Land the driver and analyzer under `test/`, alongside the existing `smoke`
  and `drill` lanes, wired into the `justfile` the way they are. Playwright is
  already in the tree for the harness quests, so prefer it over a bespoke CDP
  driver if the switch is cheap; if it is not, say so and keep CDP.
- Keep the instrumentation ad-hoc for now. The probes stay a debug surface, not
  public API; promoting them is [Latency
  ledger](/quest/m2/latency-ledger.md), which nothing here waits on.
- The metric schema is the deliverable that outlives this quest, because the
  native lane and the ledger both have to emit the same thing. It is a
  contract, so write it as one:
  - Every counter defined, not just the two that are obvious. An underrun is a
    quantum the ring could only partly fill; a skip-ahead is a re-anchor that
    discarded buffered audio; short quanta and discarded samples need the same
    treatment rather than being left to the reader.
  - Units and clock domain stated once. Durations in one unit, and every
    timestamp on a named clock, so a browser value and a native value are the
    same measurement. Naming the clock is not enough on its own: the stages span
    the publisher, the relay and the viewer, which are separate processes and
    often separate machines, so either reduce every timestamp to one common
    monotonic reference or define the calibration and drift correction that maps
    between them. The ledger's sum-to-end-to-end identity is arithmetic across
    these values, and it is meaningless if they sit on unaligned clocks.
  - Aggregation stated per metric: a count over the run, a max, or a
    percentile. "Underruns: 3" and "underruns: 3/s" grade differently.
  - Stages as exclusive, non-overlapping spans (capture, encode, publish flush,
    network, jitter buffer, decode, render). The ledger's sum-to-end-to-end
    identity is unimplementable if two stages can claim the same milliseconds,
    and an unaccounted remainder is the finding, so give it a name too.
- Extract the seeded shaper from [Impaired
  path](/quest/m0/transport-impairment-profile.md) into something that runs as
  its own process in front of a relay, so this harness and the drills share one
  impairment implementation. Assert the shaper actually treated traffic: a
  profile that silently did nothing turns an impaired run into an unimpaired
  pass.
- Profiles: near-zero, mild, bursty (the flush-span shape from #3477), and a
  step change that forces the target to move mid-run. Fixed seeds, recorded
  with the results.
- One profile runs a fixed delay preset rather than auto, as the control. Every
  other profile exercises adaptation, so without this the whole matrix can pass
  while a fixed preset regresses, and fixed presets are what a viewer lands on
  today.
- Budgets in one checked-in file, keyed by the whole matrix row and not by
  profile alone: runtime, codec, sample rate, profile, and the ring path, since
  each of those moves the expected floor. A profile-only key silently grades
  one row against another's threshold. Tightening a budget is then a visible
  diff and loosening one needs a reason in review.
- Trim the released ndjson traces from `rt-audio-traces-2026-09-06` into a
  fixture and replay them too, so a real recorded arrival pattern is graded
  next to the synthetic profiles.
- Add the lane to `nightly.yml`, and extend its header comment with why this
  one is not a PR gate.

## Required

- [Impaired path](/quest/m0/transport-impairment-profile.md) - the seeded shaper this puts in front of the relay
