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

## Related

- [Calibrate the paired nightly benchmarks](/quest/m1/bench-ci.md) - owns the gate's threshold if the Nightly flags this first
