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
  already owned elsewhere stay there: the
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
[interop green](/quest/m0/interop-green/README.md). Fixed on
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

The mesh replay's violations with no known entry on 2026-11-19 and 11-20
came from debug-built relays starved for CPU on the 4-core runner, and from
a strand counted again at every later event; both fixed in
[moq-dev/moq#9236](https://github.com/moq-dev/moq/pull/9236). Its `shown`
rows (a relay still showing a broadcast after its source ended) appeared only
in those starved runs and did not reproduce locally. A failed replay job now
uploads its relay and reader logs, so read those if `shown` recurs.

Accepted 2026-11-22, from run
[45035580806](https://github.com/moq-dev/moq/actions/runs/45035580806): the
lazy CMAF chunk of `js/watch` and `js/watch-ui` grew from 5,783 to 6,249 bytes
raw (2,491 to 2,627 gzip, 2,281 to 2,403 Brotli), all in
`js/hang/src/cmaf/decode.ts`. The cause is
[moq-dev/moq#9344](https://github.com/moq-dev/moq/pull/9344)'s esds parser, which
now skips the ES_Descriptor's optional fields and refuses truncated
descriptors, so AAC with those flags set gets its decoder config. The
thresholds are unchanged: the failed report became the next daily baseline,
and 6,249 is 8% over the 5,783 that will be the 30-day anchor, inside its 10%.

Triaged 2026-11-23 (maintainer away; agent recommendation, for review), from runs
[44767045952](https://github.com/moq-dev/moq/actions/runs/44767045952),
[44916717292](https://github.com/moq-dev/moq/actions/runs/44916717292), and
[45035580806](https://github.com/moq-dev/moq/actions/runs/45035580806): one more child,
the mesh replay's reader stranded after every link of its source's relay reopened
(11-21's `ring6-4`). The Firefox player's `replacement` failures went to
[interop green](/quest/m0/interop-green/README.md). Fixed on `main` since: features
([moq-dev/moq#9428](https://github.com/moq-dev/moq/pull/9428)), route-sweep
([moq-dev/moq#9200](https://github.com/moq-dev/moq/pull/9200)), the 11-20 and 11-21 mutants
([moq-dev/moq#9294](https://github.com/moq-dev/moq/pull/9294),
[moq-dev/moq#9312](https://github.com/moq-dev/moq/pull/9312)), cli-export
([moq-dev/moq#9311](https://github.com/moq-dev/moq/pull/9311)), the Gate's moq-ffi dial
([moq-dev/moq#9431](https://github.com/moq-dev/moq/pull/9431)), the audit
([moq-dev/moq#9429](https://github.com/moq-dev/moq/pull/9429)), the benches
([moq-dev/moq#9099](https://github.com/moq-dev/moq/pull/9099),
[moq-dev/moq#9235](https://github.com/moq-dev/moq/pull/9235),
[moq-dev/moq#9251](https://github.com/moq-dev/moq/pull/9251),
[moq-dev/moq#9305](https://github.com/moq-dev/moq/pull/9305),
[moq-dev/moq#9310](https://github.com/moq-dev/moq/pull/9310),
[moq-dev/moq#9433](https://github.com/moq-dev/moq/pull/9433),
[moq-dev/moq#9434](https://github.com/moq-dev/moq/pull/9434)), drain
([moq-dev/moq#9213](https://github.com/moq-dev/moq/pull/9213)), drill sensitivity
([moq-dev/moq#9292](https://github.com/moq-dev/moq/pull/9292)), and the 11-20 mesh replay
([moq-dev/moq#9236](https://github.com/moq-dev/moq/pull/9236)). Owned by open PRs or
quests: the 11-22 resume mutants
([moq-dev/moq#9393](https://github.com/moq-dev/moq/pull/9393),
[moq-dev/moq#9750](https://github.com/moq-dev/moq/pull/9750)), quic-interop's
lossy handshake cells ([moq-dev/moq#9445](https://github.com/moq-dev/moq/pull/9445),
[moq-dev/moq#9447](https://github.com/moq-dev/moq/pull/9447),
[moq-dev/moq#9453](https://github.com/moq-dev/moq/pull/9453)), the TLA+ failover model's
deadlock ([moq-dev/moq#9430](https://github.com/moq-dev/moq/pull/9430)), and quic-sim's
live-edge budget ([moq-dev/moq#9439](https://github.com/moq-dev/moq/pull/9439)). Infra:
the Benchmark site's artifact listing missed the `github-pages` artifact uploaded a second
before (11-20 and 11-22, so no longer a one-off; re-check if it fails a third night), and
the 11-20 Windows C and Cpp release builds failed on Chocolatey not finding `nasm` 3.2.0,
not a runner bash startup failure as recorded above.

Accepted the same day: `routes/lagging` doubled on 11-20 (`100c_1l` +92%, `100c_100l`
+89%), from [moq-dev/moq#9179](https://github.com/moq-dev/moq/pull/9179). Measured locally
in alternating pairs: 2.62 µs at its parent, 4.82 µs with it, and 4.8 to 4.9 µs on every
later commit. Its rule that a request follows a claim only through a neighbour whose copy
holds it makes the lagging stream's older batch a real change, which marks the claim dirty
so the relay works its ways out again, a constant cost per step. The bench's comment now
says so. Nightly compares against a base that already carries the cost, so it will not
flag it again.

Triaged 2026-11-25 (pre-0.22 recurring audits), from run
[45242597151](https://github.com/moq-dev/moq/actions/runs/45242597151) (`b29b90be29`):
`Tests (test resume-model)` at seed `1465722607` was fixed by
[moq-dev/moq#9509](https://github.com/moq-dev/moq/pull/9509); `Nightly (JS coverage)`'s
`widened_max_age.test.ts` failure is
[moq-dev/moq#9607](https://github.com/moq-dev/moq/pull/9607); and
`Shipping sizes`' +10% `moq_ffi.dll.lib` is the Windows import library growing with the
exports [moq-dev/moq#9359](https://github.com/moq-dev/moq/pull/9359) added, while every
shipped library stayed under the 5% bound. Already owned: `Tests (rs quic-sim)`'s
`many_groups_carry_bulk_transfers_losslessly` by
[moq-dev/moq#9439](https://github.com/moq-dev/moq/pull/9439), and the paired benchmark
regressions by [perf-recheck](/quest/m3/release/perf-recheck.md).

Triaged 2026-11-26 (maintainer, /quest-plan), from Nightly runs
[45667362304](https://github.com/moq-dev/moq/actions/runs/45667362304) (`420b8dcb0d`) and
[45867457463](https://github.com/moq-dev/moq/actions/runs/45867457463) (`4fba5df73f`),
scheduled Interop [45873739689](https://github.com/moq-dev/moq/actions/runs/45873739689), and
TS [45872586504](https://github.com/moq-dev/moq/actions/runs/45872586504). New children: the
JS resume model's failed final group, the route sweep's job split, the bursty audio rows, and
the 11-26 paired regressions; two Interop children went to
[interop green](/quest/m0/interop-green/README.md), and the Pages deploy to
[CI](/quest/m0/ci/README.md). The macOS moq-cli tarball's crc-fast link failure is fixed by
[moq-dev/moq#9728](https://github.com/moq-dev/moq/pull/9728). Fixed on `main` since: the track
bench ([moq-dev/moq#9635](https://github.com/moq-dev/moq/pull/9635); 11-26 failed only on its
base), `widened_max_age` ([moq-dev/moq#9607](https://github.com/moq-dev/moq/pull/9607)),
cli-export ([moq-dev/moq#9662](https://github.com/moq-dev/moq/pull/9662)), and the TS
congested path ([moq-dev/moq#9621](https://github.com/moq-dev/moq/pull/9621)). Owned by open
PRs: Windows' CRLF crate names ([moq-dev/moq#9545](https://github.com/moq-dev/moq/pull/9545))
and quic-interop's lossy handshakes ([moq-dev/moq#9445](https://github.com/moq-dev/moq/pull/9445),
[moq-dev/moq#9447](https://github.com/moq-dev/moq/pull/9447),
[moq-dev/moq#9453](https://github.com/moq-dev/moq/pull/9453)); the 11-26 mutants went to
[moq-dev/moq#9750](https://github.com/moq-dev/moq/pull/9750). Dropped as one-night
failures (re-check if one repeats): `benchmark_receive_windows`' `group timed out` on a
non-skipped cell, the Firefox player's `after resume` tone at 65/75 samples, Interop's
Chromium `volume 0.25` at 27/31 samples, Interop's `finish go -> *` gst reader on 11-25, and
the supplemental benchmarks' 55-minute step timeout on 11-25 (28 minutes compiling).

Bisected 2026-11-27 (maintainer away; agent recommendation, for review), the paired
regressions Nightly [45867457463](https://github.com/moq-dev/moq/actions/runs/45867457463)
confirmed, still on `main` at `83e10f008a`. Counted in user instructions per iteration on one
pinned core, since wall time on the shared host varied by up to 30% between runs:

- `moq-audio/capture` `capture_fanout/*`: from
  [moq-dev/moq#9675](https://github.com/moq-dev/moq/pull/9675). Once kio left the workspace,
  `cargo hakari` unified its test-only `sim` and `manual` features into the workspace-hack,
  so every bench and test build compiled kio's per-poll accounting: 13 more instructions per
  read, +3.9% to +9.7% per cell. Release builds never carried it. [moq-dev/moq#9747](https://github.com/moq-dev/moq/pull/9747) excludes kio from the
  workspace-hack (`.config/hakari.toml`), which restores every cell's count to the base's.
- `moq-net/session` `session_delivery_group_cost_cpu/moq-lite-07-wip/.../publishers=16/.../viewers=1`:
  from [moq-dev/moq#9529](https://github.com/moq-dev/moq/pull/9529), not #9686 (about 1,000
  instructions). It cost every version 6 allocations and about 11,000 instructions (+2.3%)
  per viewer-group: 487,300 at the base, 500,200 on `main`. [moq-dev/moq#9747](https://github.com/moq-dev/moq/pull/9747) removes the
  allocations (a `Vec` per arrival walk, and a group consumed only to judge it stale),
  leaving 498,500. Accepted: the remaining 2.3% pays for serving held groups in Group
  Order, where a newest-first read judges the oldest group it holds before handing out a
  newer one. Fastest-of-six cycles agree: +2.4% on `main`, +1.4% with #9747, not +11%.

Triaged 2026-11-27 (maintainer, /quest-plan). No scheduled run had finished since 11-26's
[45867457463](https://github.com/moq-dev/moq/actions/runs/45867457463), and the 11 dispatched
runs since are the automated bisect of its paired regressions, at commits from before the 11-27
fixes, so the triage read them only for failures that predate no fix. New children: the Firefox
player's lifecycle steps, which failed 6 of 15 runs at a different step almost every time
([interop green](/quest/m0/interop-green/README.md); every failure predates
[moq-dev/moq#9561](https://github.com/moq-dev/moq/pull/9561)'s 100 ms audio groups, so
[moq-dev/moq#9809](https://github.com/moq-dev/moq/pull/9809) deleted it), and the MediaCodec
keyframe test (fixed by [moq-dev/moq#9814](https://github.com/moq-dev/moq/pull/9814)). Fixed by
[moq-dev/moq#9785](https://github.com/moq-dev/moq/pull/9785): `Features (rs features)`'s
`concurrent_ephemeral_groups_all_bind`, where an ephemeral group failed with `Overlap` on a port
another address's group had locked. Fixed on `main` since: the resume model's seed `478520378`
(passes at `f7a41c3ea5`), moq-ffi's `server_client_run_end_draft22`
([moq-dev/moq#9722](https://github.com/moq-dev/moq/pull/9722)), and the benches, cli-export, and
route-sweep, which failed only at commits before
[moq-dev/moq#9747](https://github.com/moq-dev/moq/pull/9747),
[moq-dev/moq#9662](https://github.com/moq-dev/moq/pull/9662), and
[moq-dev/moq#9751](https://github.com/moq-dev/moq/pull/9751). Owned by open PRs: Audio quality
([moq-dev/moq#9783](https://github.com/moq-dev/moq/pull/9783)), mutants
([moq-dev/moq#9750](https://github.com/moq-dev/moq/pull/9750),
[moq-dev/moq#9623](https://github.com/moq-dev/moq/pull/9623)), Windows
([moq-dev/moq#9545](https://github.com/moq-dev/moq/pull/9545)), and quic-interop
([moq-dev/moq#9445](https://github.com/moq-dev/moq/pull/9445),
[moq-dev/moq#9447](https://github.com/moq-dev/moq/pull/9447),
[moq-dev/moq#9453](https://github.com/moq-dev/moq/pull/9453)). Scheduled run
[46038678705](https://github.com/moq-dev/moq/actions/runs/46038678705) (`f7a41c3ea5`), the first
with those fixes, goes to the next triage.

Triaged 2026-11-28 (maintainer, /quest-plan), from Nightly
[46038678705](https://github.com/moq-dev/moq/actions/runs/46038678705) (`f7a41c3ea5`), scheduled TS
[46044376056](https://github.com/moq-dev/moq/actions/runs/46044376056) and Interop
[46045301725](https://github.com/moq-dev/moq/actions/runs/46045301725) and
[46166329015](https://github.com/moq-dev/moq/actions/runs/46166329015), and Cache's
`Warm (macos-latest)` since `f101d7908e`. Fixed in PRs from this triage: the lite-07 fetch cells
of the Rust and relay session benches and the JS `publisher-update` hang, both from
[moq-dev/moq#9687](https://github.com/moq-dev/moq/pull/9687)
([moq-dev/moq#9855](https://github.com/moq-dev/moq/pull/9855)); `TS long recovery`'s fault model,
from [moq-dev/moq#9561](https://github.com/moq-dev/moq/pull/9561)'s 100 ms audio groups
([moq-dev/moq#9856](https://github.com/moq-dev/moq/pull/9856)); the macOS dev shell, whose
nixpkgs from the quest v0.31.0 bump fails Darwin swift-collections and no binary cache carries a
Darwin `swift-format` at it or at nixpkgs-unstable's head
([moq-dev/moq#9857](https://github.com/moq-dev/moq/pull/9857)); Interop's open-GOP fixture,
from [moq-dev/moq#9794](https://github.com/moq-dev/moq/pull/9794)
([moq-dev/moq#9858](https://github.com/moq-dev/moq/pull/9858)); and the three easy surviving
`resume.rs` mutants (MUTANTS_PR). New child, in m3 by decision:
[resume.rs's back-cut mutants](/quest/m3/resume-cut-mutants.md), the other 16. Owned by open
PRs or quests: Windows' CRLF crate names
([moq-dev/moq#9545](https://github.com/moq-dev/moq/pull/9545), a draft since 11-24),
quic-interop's moq-quic client against quic-go `handshakeloss`, a runner miscount
([moq-dev/moq#9445](https://github.com/moq-dev/moq/pull/9445), which now conflicts with `main`),
and Chromium lite-06 stream credit
([lite-06 lost reset](/quest/m0/test-flakes-5/lite06-lost-reset-hang.md)). Dropped as
one-night failures (re-check if one repeats): `moq-relay/http_auth`'s paired
`relay_http_admission` +10 to 15% (each night flags a different unrelated bench at that size),
the TS congested path (passed on 11-28), and `relay_fill_javascript`'s lite-07
`below the start` (passed 10 of 10 locally at `f7684041f5`).

Accepted the same day: `Shipping sizes`' `js/hang` grew 5.4% raw (678,554 to 715,316 bytes;
brotli 183,890 to 193,538) over the 5% day-over-day bound, two days of growth since the 11-26
job skipped. About 29.5 KB is `@moq/net` (`track`, the lite subscriber and publisher, `resume`)
and 7.4 KB is `@moq/hang` (`health.js`, `util/aac.js`, the `cts` and `raw` containers), with no
single culprit; the failed report is the next baseline. The trend (`js/hang` +20% since 11-18)
went to the [cost audit](/quest/m3/audit-cost.md).
Today's run [46162157084](https://github.com/moq-dev/moq/actions/runs/46162157084) goes to the
next triage; it already shows `moq-quic/wire` `wire_decode/padded/packets_64` +11.4% and a
browser benchmark smoke `#writeFrame` error.
