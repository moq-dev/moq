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
  native lane has to emit the same thing. Name each stage once (capture,
  encode, publish flush, network, jitter buffer, decode, render) and define the
  glitch counters exactly: an underrun is a quantum the ring could only partly
  fill, a skip-ahead is a re-anchor that discarded buffered audio.
- Extract the seeded shaper from [Impaired
  path](/quest/m0/transport-impairment-profile.md) into something that runs as
  its own process in front of a relay, so this harness and the drills share one
  impairment implementation. Assert the shaper actually treated traffic: a
  profile that silently did nothing turns an impaired run into an unimpaired
  pass.
- Profiles: near-zero, mild, bursty (the flush-span shape from #3477), and a
  step change that forces the target to move mid-run. Fixed seeds, recorded
  with the results.
- Budgets in one checked-in file keyed by profile, so tightening one is a
  visible diff and loosening one needs a reason in review.
- Trim the released ndjson traces from `rt-audio-traces-2026-09-06` into a
  fixture and replay them too, so a real recorded arrival pattern is graded
  next to the synthetic profiles.
- Add the lane to `nightly.yml`, and extend its header comment with why this
  one is not a PR gate.

## Required

- [Impaired path](/quest/m0/transport-impairment-profile.md) - the seeded shaper this puts in front of the relay
