# [M] Test cost audit

## Goal

The slowest, flakiest, and least valuable tests are deleted by default, and
the ones that earn their keep are sped up, fixed, or moved to nightly, so the
suite costs what it earns. Workflows and jobs belong to the
[CI workflow audit](/quest/m2/audit-ci.md).

## Plan

Recurs once per major release (decided 2026-10-19). The PR that finishes
this run also adds the next one with the same file name at the end of the next
milestone's `Required` (m0 to m1, m1 to m2). The successor keeps the Goal
and this paragraph; the rest of its Plan is rewritten with what this run
learned: baselines, results, and what to check first. A successor placed in m2
also lists the [release cut](/quest/m2/release/cut.md) under `Required`, so it
waits for the release in flight to ship. Only `/quest-delete` ends the
recurrence. Fixes land in the run's PR, one commit each, rather than as follow-ups.
Only a change that needs a maintainer decision (public API, scope) or is
independently landable and too large to review alongside the rest becomes a
follow-up through `/quest-plan`.

Default to removing (decided 2026-11-01, maintainer): whatever doesn't earn its
cost is deleted inline, and the PR body lists each deletion with its measured
cost and why it wasn't worth it. Only what earns its keep is refactored to be
cheaper instead. Removing a public API or a user-facing feature stays a
follow-up.

The off-cycle m1 pruning run (2026-11-01) refreshes this baseline in place;
it does not finish this recurring run, move its schedule, or create a successor.

Measure nextest's per-test elapsed time as slot time, not CPU time. Sum only
one invocation: the full gate also runs feature-specific selections, and a
failed run may print the failed test twice. Keep the invocation's test count,
profile, features, thread count and completion state beside its timing. Repeat
local measurements under `quest gate`; concurrent work makes one sample
unreliable. Pin whole-suite comparisons to the same eight CPUs. A roughly
three-second swing can be QUIC draining rather than computation. The `ci`
profile marks a test slow at 60 seconds and uses zero retries.

Current hosted baseline:

- [2026-10-31 Nightly, gate job](https://github.com/moq-dev/moq/actions/runs/41204380716/job/124546373689):
  the main nextest invocation passed 10,645 tests, skipped 115, and took
  196.586 seconds. Summed test slot time was 768.723 seconds; the 27 tests
  above five seconds used 258.848 of it. By crate: moq-mux 193.621 seconds,
  moq-net 142.320, moq-relay 80.813, moq-quic 48.398, moq-tokio 46.230,
  moq-uring 42.835, and moq-cli 37.359. Other invocations in that job are
  excluded from those sums. These hosted ARM64 timings replace the old
  9,312-test queue baseline for ranking, not for a same-machine speedup claim.
- [2026-11-01 Nightly, gate job](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868517727):
  9,163 of 10,891 tests ran in 249.218 seconds before the relay stack test
  aborted. This is an incomplete selection, not a new whole-suite baseline.
  [moq-dev/moq#7595](https://github.com/moq-dev/moq/pull/7595) fixes the
  private configuration size without weakening that test.
- [2026-10-31 CLI export lane](https://github.com/moq-dev/moq/actions/runs/41204380716/job/124546373649):
  14 passed in 38.774 seconds, using 124.897 summed test seconds. The clean
  and bufferbloated redundant-importer arms cost 18.669 and 26.278 seconds.
  They exercise real process failover and impaired delivery. The route
  identity change in [moq-dev/moq#7489](https://github.com/moq-dev/moq/pull/7489)
  owns changes to that harness; do not duplicate them here.
- [2026-11-01 Rust benchmark-test lane](https://github.com/moq-dev/moq/actions/runs/41313466339/job/124868518054):
  all 15 selected benchmark tests passed, using 114.177 summed seconds.
  Receive-window sweeps used 80.370 seconds, raw audio 11.341. These measure
  different transport, layout and fanout cases, rather than repeating one
  behavior assertion.

Local default-feature workspace selection on `21d6da9fc1`, pinned to CPUs
0-7 with 24 nextest threads and the `ci` profile: 10,623 tests ran in
50.433 seconds, using 997.332 summed seconds. One failed:
`backend::quic_client_close_drains_finished_track` with `Unroutable`, already
tracked by [moq-dev/moq#7590](https://github.com/moq-dev/moq/pull/7590).
This is not `just ci`'s device/runtime feature selection. Before the cut, the
99-test import/report selection passed; the deletion candidate cost 7.763
seconds in the full run and 7.152 in that selection. Do not claim either run
as a passing full gate or attribute whole-suite speed to this deletion.
After removal, the matching import/report selection passed all 98 remaining
tests. The standard `just ci` attempt stopped in Cargo documentation on mbx
`E2BIG`; [moq-dev/moq#7594](https://github.com/moq-dev/moq/pull/7594) owns it.
No environment or Cargo launcher bypass was used.

The m1 cut removes `report::import_reports_a_silent_video_pid` and its private
TS rewriting helpers. It cost 6.932 and 6.948 seconds in the two hosted gate
runs. The kept mid-import report test already checks the CLI-to-relay-to-report
path; `liveness_steps_over_a_corrupt_pcr_and_the_wrap` covers silence and
resumption directly, and the importer and stats tests cover stalled video
beside sparse data. The removed process test added a paced five-second clip,
two child processes, and another TS suppressor to repeat those properties.
The mid-import report and late-sink tests stay, as do the two stdin-EOF tests:
ongoing reports, cached final reports, catalog finish, and media announced only
at EOF are different process boundaries.

Check first:

- Account for [moq-dev/moq#6987](https://github.com/moq-dev/moq/pull/6987)
  before touching the TS export/jitter simulations, routing reference model,
  JSON budgets, QUIC simulation payloads, or other CPU cuts it owns. Its
  repeated cohort measurements are 61.02 and 67.49 CPU-seconds; neither is
  this audit's saving. The hosted drift cases still cost 12 to 19 seconds
  each before those changes land.
- [moq-dev/moq#7549](https://github.com/moq-dev/moq/pull/7549) owns the
  refused-fill test and its scaling benchmark. Recovery fixture caching,
  tracing isolation and stats oversize cuts have already landed; its body
  identifies their owners. Do not repeat the old follow-up list.
- Compressed-cache bounds earn their cost: the long compressed group used
  22.150 and 42.928 hosted seconds, the large compressible batch 6.608 and
  10.849. They test stored-tail shedding while a plain reader lags, and
  bounded inflation when tiny slices describe large frames. Smaller codec
  round trips do not test either resource bound. Preserve those assertions.
- The three io_uring endpoint timeout/backlog tests used 30.284 seconds
  together on October 31. Their unanswered handshake, pending admission and
  completed-but-unaccepted connection cases are distinct. Keep them without
  adding a timeout knob. [moq-dev/moq#7593](https://github.com/moq-dev/moq/pull/7593)
  owns ring grouping and fixture startup diagnostics, not test deletion.
- Macro UI compile failures, ACME issuance/cache/rate-limit cases, and
  GStreamer reconnects have distinct compiler, TLS, persistence and real
  socket boundaries. Their elapsed cost alone is not evidence of redundancy.
- Classify failures before calling them flakes. October 30 and 31's feature
  jobs stopped on an unused `moq-tokio::connect::Config::with_url` method;
  November 1's gate and all-features run aborted on the relay stack. Neither
  is intermittent evidence against a test. Existing flake quests retain
  their ownership; do not delete their tests to green the audit.

## Required

- [Cut and publish the prepared breaking release](/quest/m2/release/cut.md) - the release in flight ships before this run starts
