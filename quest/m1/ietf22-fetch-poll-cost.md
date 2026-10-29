# [M] moq-transport-22 FETCH costs what it did before the waiter refactor

## Goal

A moq-transport-22 FETCH of 256-frame groups costs no more instructions than
it did before [moq-dev/moq#7042](https://github.com/moq-dev/moq/pull/7042), or
the remaining cost is attributed to work the refactor needs and recorded in
its PR. Other versions and group sizes don't regress.

## Plan

Found 2026-10-28 by the paired-relay measurements in
[moq-dev/moq#7141](https://github.com/moq-dev/moq/pull/7141): #7042
(`6eff65506e`, "transport polls take kio waiters through &self") adds +12.8%
instructions on moq-transport-22 FETCH of 256-frame groups (8.02M to 9.04M per
round). The commit before it measures the same as the head. It landed after the
range the 2026-10-28 Nightly compared, so the next paired Nightly may flag it.

Decided 2026-10-28 (maintainer): m1, not a Nightly blocker.

- Attribute with instruction counts (`perf stat --no-scale` pinned to one core
  type, or callgrind), not wall time: the hosted runners and a busy host are
  too noisy for a 13% delta. Give each revision its own `CARGO_TARGET_DIR`.
- Find which poll path the refactor made more expensive on draft 22's FETCH
  (per-call waiter registration, extra polls, or a lost fast path), and fix it
  at the cause without reverting the `&self` waiter API.
- Sweep the relay session bench's FETCH cases across lite-07, draft 18 and
  draft 22, at 4- and 256-frame groups, and record before and after.

Public API: none. Wire: none.

### Checkpoint: 2026-10-29

Implementation attempt: [moq-dev/moq#7262](https://github.com/moq-dev/moq/pull/7262).
Current main was `c3fcab120688cb00e592895c5930f601c34c5cdb`.
The regression has **not** been reproduced on that revision. The numbers above
remain evidence from [moq-dev/moq#7141](https://github.com/moq-dev/moq/pull/7141),
not measurements from this attempt.

- Hardware instruction counting works: `taskset -c 0 perf stat --no-scale
  -e cpu_core/instructions/u -- true` counted 1,060,017 user instructions,
  with 100% counter runtime. This is only a counter-availability probe.
  This hybrid host lists CPUs 0-7 under `/sys/devices/cpu_core/cpus`.
- The first build, `quest gate env
  CARGO_TARGET_DIR=.tmp/fetch-poll/target-main cargo bench --locked -p moq-net
  --features fuzz --bench session --no-run`, never passed admission:
  `quest gate: waiting for the load average to fall`. It was terminated before
  Cargo started. No benchmark binary, workload counts, or profile was produced.
- No production code, waiter API, benchmark thresholds, or Nightly settings
  changed. No optimization or necessary residual cost can be justified yet.

Remaining work:

- First reproduce against current main, the waiter refactor
  `6eff65506e817ca82c6ee3d408c55066cf811510`, and its parent, using separate
  source exports and target directories. Keep the workload and build settings
  identical. Count a known number of steady rounds, with setup excluded or
  separately accounted for, pinned to one core type; record counter runtime
  and reject multiplexed or uncounted samples.
- The shared session bench currently uses lite-06, lite-07-wip, and draft 22
  in `VERSIONS`; draft 18 is missing from the quest's requested sweep. Cover
  lite-07-wip, draft 18, and draft 22 FETCH at 4 and 256 frames, retaining
  independent publisher counts 1/4 and viewer counts 1/16. Use the relay bench
  for the final sweep and the model bench to separate relay overhead.
- Attribute the per-frame cost before changing it. Source inspection shows
  FETCH header and payload reads now create `kio::wait` futures, and the memory
  transport polls session close before each channel read. Whether waiter
  registration, extra polls, or another path explains the reported delta is
  unmeasured. Preserve the `&self` waiter API, fix only a measured cause, extend
  its benchmark, and explicitly account for any necessary remaining cost.

## Related

- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - owns the gate's threshold if the Nightly flags this first
