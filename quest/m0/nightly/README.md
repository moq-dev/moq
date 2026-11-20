# [M] Nightly passes on main

## Goal

The scheduled Nightly on `main` passes end to end, so a red run means a new
break instead of noise. Nightly is the backstop for what the PR gate skips on
purpose (benches, examples, feature shapes; see
[moq-dev/moq#5531](https://github.com/moq-dev/moq/pull/5531)), and it has
failed every night since at least 2026-10-19, which let a bench and two
example breaks reach `main` unnoticed.

## Plan

Decided 2026-10-24 (maintainer), from a triage of run
[40090959108](https://github.com/moq-dev/moq/actions/runs/40090959108): one
child per distinct cause, each landing on its own. The PR gate keeps
skipping benches and examples; a green Nightly catches them within a day.

Decided 2026-10-27 (maintainer), from the coverage audit's triage of run
[40506938673](https://github.com/moq-dev/moq/actions/runs/40506938673): more
children for the causes nothing owned.

This README's own work, after the children:

- Confirm a scheduled run on `main` passes; never dispatch Nightly. A
  triage counts a cancelled job as failed: a workload that overruns fails at
  its step's timeout, so a job cancelled at its own timeout hung outside it. Failures
  already owned elsewhere stay there: the audio near-zero p95 row belongs to
  [audio playout](/quest/m2/audio-playout/README.md), the
  macOS `receive_credit` timeout has no owner since its quest was deleted in
  [moq-dev/moq#6732](https://github.com/moq-dev/moq/pull/6732), so plan one under
  a test flakes epic if it recurs, and the paired bench
  threshold is calibrated in [Paired comparison](/bench/README.md#paired-comparison). The moq-net benchmark
  fixtures were repaired in [moq-dev/moq#7940](https://github.com/moq-dev/moq/pull/7940).
  The receive-windows benchmark's overcommitted cells, skipped since
  [moq-dev/moq#7838](https://github.com/moq-dev/moq/pull/7838), moved to
  [m1](/quest/m3/receive-windows-evicted.md) on 2026-11-04 (maintainer).

Decided 2026-11-05 (maintainer, /quest-plan): catch the next moq-net bench
break in Nightly's measured coverage only. Do not add a test-mode bench run
to the PR gate; the full run costs about 17 minutes per PR.

Public API: none. Wire: none.

Decided 2026-11-09 (maintainer, /quest-complete), from the triage of runs
[42377867523](https://github.com/moq-dev/moq/actions/runs/42377867523) and
[42487375545](https://github.com/moq-dev/moq/actions/runs/42487375545): four
more children for the causes nothing owned. Dropped: one-night bench moves
near the paired threshold (re-check if one repeats), a one-off
`remote error: 52` in the browser bench smoke, and failures main or
[moq-dev/moq#8290](https://github.com/moq-dev/moq/pull/8290) already fixes.

Decided 2026-11-13 (maintainer, /quest-plan), from the triage of run
[43447849289](https://github.com/moq-dev/moq/actions/runs/43447849289): four
more children, all in m0. Owned elsewhere: the fuzz build
([moq-dev/moq#8657](https://github.com/moq-dev/moq/pull/8657)), the Python
audit ([moq-dev/moq#8648](https://github.com/moq-dev/moq/pull/8648)), the size
report's output ([moq-dev/moq#8654](https://github.com/moq-dev/moq/pull/8654)),
the firefox resume ([moq-dev/moq#8635](https://github.com/moq-dev/moq/pull/8635)),
and the chromium `av_lag` rows
([moq-dev/moq#8676](https://github.com/moq-dev/moq/pull/8676)).
After #8635 landed, run
[43601796176](https://github.com/moq-dev/moq/actions/runs/43601796176) still
failed the firefox resume, now on audio arriving late rather than trailing:
[moq-dev/moq#8799](https://github.com/moq-dev/moq/pull/8799) fixed that.


Decided 2026-11-17 (maintainer, /quest-plan), from the triage of runs
[43909101837](https://github.com/moq-dev/moq/actions/runs/43909101837) and
[44127032469](https://github.com/moq-dev/moq/actions/runs/44127032469): two
more children, the lite-07 route hang and the credential-established reconnect
signal. Fixed in PRs from the same triage: fuzz path
([moq-dev/moq#9015](https://github.com/moq-dev/moq/pull/9015)), cli-export
([moq-dev/moq#9017](https://github.com/moq-dev/moq/pull/9017)), audit ignores
([moq-dev/moq#9012](https://github.com/moq-dev/moq/pull/9012)), JS coverage
([moq-dev/moq#9013](https://github.com/moq-dev/moq/pull/9013)), Gate cost
([moq-dev/moq#9014](https://github.com/moq-dev/moq/pull/9014)), Tests (gate)
disk ([moq-dev/moq#9033](https://github.com/moq-dev/moq/pull/9033)), benches
([moq-dev/moq#9020](https://github.com/moq-dev/moq/pull/9020),
[moq-dev/moq#9021](https://github.com/moq-dev/moq/pull/9021),
[moq-dev/moq#9022](https://github.com/moq-dev/moq/pull/9022)), and mutants
([moq-dev/moq#9019](https://github.com/moq-dev/moq/pull/9019)).

Decided 2026-11-18 (maintainer, /quest-plan), from the triage of runs
[44127032469](https://github.com/moq-dev/moq/actions/runs/44127032469) and
[44342367698](https://github.com/moq-dev/moq/actions/runs/44342367698): four
more children (JS resume window, audio-quality p50 ceilings, publisher drop
cost, resume mutants). Owned elsewhere: the firefox player by
[moq-dev/moq#9141](https://github.com/moq-dev/moq/pull/9141), OBS by
[moq-dev/moq#9016](https://github.com/moq-dev/moq/pull/9016), and fixes in PRs
from the same triage: benches
([moq-dev/moq#9089](https://github.com/moq-dev/moq/pull/9089),
[moq-dev/moq#9090](https://github.com/moq-dev/moq/pull/9090),
[moq-dev/moq#9098](https://github.com/moq-dev/moq/pull/9098),
[moq-dev/moq#9099](https://github.com/moq-dev/moq/pull/9099)), features
([moq-dev/moq#9091](https://github.com/moq-dev/moq/pull/9091),
[moq-dev/moq#9092](https://github.com/moq-dev/moq/pull/9092)), quic-interop
([moq-dev/moq#9096](https://github.com/moq-dev/moq/pull/9096),
[moq-dev/moq#9097](https://github.com/moq-dev/moq/pull/9097)), fuzz
([moq-dev/moq#9105](https://github.com/moq-dev/moq/pull/9105),
[moq-dev/moq#9106](https://github.com/moq-dev/moq/pull/9106)), and route-sweep
([moq-dev/moq#9108](https://github.com/moq-dev/moq/pull/9108)). The JS audit's
picomatch advisory was withdrawn upstream.

Decided 2026-11-19 (maintainer, /quest-plan), from the triage of runs
[44562804083](https://github.com/moq-dev/moq/actions/runs/44562804083) and
[44342367698](https://github.com/moq-dev/moq/actions/runs/44342367698): seven
more children (push-to-talk native listener, drain exit, mesh replay
violations, shared-bench regressions, resume and node mutants, Miri's
boottime clock, the ingest fuzz bound). Owned elsewhere: quic-interop's
`handshakecorruption`
([moq-dev/moq#9097](https://github.com/moq-dev/moq/pull/9097)), the JS
benches ([moq-dev/moq#9099](https://github.com/moq-dev/moq/pull/9099)),
features ([moq-dev/moq#9091](https://github.com/moq-dev/moq/pull/9091),
[moq-dev/moq#9092](https://github.com/moq-dev/moq/pull/9092)), route-sweep's
empty filter ([moq-dev/moq#9200](https://github.com/moq-dev/moq/pull/9200)),
the Rust bench targets' step timeout
([moq-dev/moq#9199](https://github.com/moq-dev/moq/pull/9199)), browser Opus
`bursty` underruns
([moq-dev/moq#9191](https://github.com/moq-dev/moq/pull/9191)), and the
Firefox lite-07 `replacement` stall, noted in
[interop green](/quest/m0/interop-green/README.md). Fixed on `main`: the JS
resume model ([moq-dev/moq#9115](https://github.com/moq-dev/moq/pull/9115))
and the relay session bench's base panic
([moq-dev/moq#9098](https://github.com/moq-dev/moq/pull/9098), clears once
the base moves past it). Dropped: the aac `step` `av_lag_ms_p50` of 5.1
against 5, a one-night move near the threshold, and the Kotlin wrapper's
Gradle plugin resolution failure, a one-off after two passing nights
(re-check either if it repeats).

Decided 2026-11-20 (maintainer, /quest-plan), from the triage of runs
[44562804083](https://github.com/moq-dev/moq/actions/runs/44562804083) and
[44767045952](https://github.com/moq-dev/moq/actions/runs/44767045952): two
more children (resume and relay route mutants, drill leak sensitivity), and
the Firefox `replacement` stall went to
[interop green](/quest/m0/interop-green/replacement-join-delay.md). Fixed on
`main`: the ingest fuzz bound
([moq-dev/moq#9216](https://github.com/moq-dev/moq/pull/9216)), Miri's
boottime clock ([moq-dev/moq#9212](https://github.com/moq-dev/moq/pull/9212)),
the earlier resume mutants
([moq-dev/moq#9217](https://github.com/moq-dev/moq/pull/9217)), and
push-to-talk ([moq-dev/moq#9214](https://github.com/moq-dev/moq/pull/9214)).
Owned by open PRs: the JS benches
([moq-dev/moq#9099](https://github.com/moq-dev/moq/pull/9099)), the Rust bench
timeout ([moq-dev/moq#9199](https://github.com/moq-dev/moq/pull/9199)), and the
container export slowdown from
[moq-dev/moq#9196](https://github.com/moq-dev/moq/pull/9196)
([moq-dev/moq#9235](https://github.com/moq-dev/moq/pull/9235)). Dropped as
one-offs: the Windows C and Cpp release builds' runner bash startup failure,
and the Benchmark site's artifact fetch (re-check either if it repeats).
