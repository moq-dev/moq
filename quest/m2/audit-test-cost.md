# [M] Test cost audit

## Goal

The slowest, flakiest, and least valuable tests are sped up, fixed, deleted,
or planned, so the suite costs what it earns. Workflows and jobs belong to the
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

Rank tests from nextest's lines in a full-selection queue log (`slow-timeout`
is 60 s under the `ci` profile). Flakes are tracked by hand in their own
quests, and `retries = 0` is deliberate. A test that sleeps on the wall clock
should mock time (AGENTS.md). Delete tests whose coverage another test already
provides. Repeat each local measurement: parallel agents load the host, so a
single run can be off by 2x. Pin to the queue VM's 8 CPUs with `taskset -c
0-7` when timing the whole suite. A test whose time differs between runs by
about 3 s is waiting out a QUIC draining period, not computing.

What the m1 run (2026-10-21) fixed: the 900 s TS export past-limit failure the
jitter buffer's tests already cover (16.6 s), the close-code tests dialing
`localhost` against an IPv4-only server (3 s to 0.5 s each on the NixOS
runner, which lists `::1` first), and the split-accept driver tests' 333 ms
initial RTT (3 s to 0.2 s each). A benchmark-only edit now selects only its
own Rust crate or JS workspace; a moq-stats bench edit had run the relay and
CLI suites. To catch the `localhost` race on a host without it, run the
suite under an `/etc/hosts` that lists `::1` first (`unshare -rm`, then a
bind mount over `/etc/hosts`); only the close-code tests regressed.
[moq-dev/moq#6192](https://github.com/moq-dev/moq/pull/6192) landed alongside:
moq-auth's re-checks on the paused clock, moq-c's encoder probe, moq-gst's and
moq-relay's idle and retry waits, and moq-cli's export process tests moved to
nightly.

Baselines from queue run
[39546261474](https://github.com/moq-dev/moq/actions/runs/39546261474), a full
selection on the 8-core runner VM: nextest ran 9,312 tests in 127 s. Their
summed time was 995 s, so the run is packed by slot time; the 48 tests over
5 s were 480 s of it. By crate: moq-cli 225 s, moq-relay 202 s, moq-mux 130 s,
moq-net 73 s, moq-tokio 59 s, moq-gst 56 s, moq-uring 41 s.

Check first:

- Test threads. Pinned to 8 CPUs on a loaded host, the whole suite took
  105 to 113 s at 8 threads (about 185% CPU), 60 s at 16 (315%), 47 s at 24
  (420%), and 41 s at 32 (500%): the suite needs about 200 CPU-seconds, and
  most slots wait on sockets, subprocesses, and wall-clock timeouts.
  The maintainer selected 24 threads for the queue in
  [CI housekeeping](/quest/m1/ci-housekeeping.md), which owns the loaded
  proof before applying it. Only the known redundant-importer flake failed,
  once, at 16 threads in this baseline.
- moq-gst's unanswered-dial close wait was fixed by
  [moq-dev/moq#6201](https://github.com/moq-dev/moq/pull/6201): the close
  stops counting an unanswered handshake as active immediately while the
  closing period still answers late packets. Do not repeat the old request
  to skip draining. [moq-dev/moq#6240](https://github.com/moq-dev/moq/pull/6240)
  retired the silent-peer quest as complete.
- moq-uring: three endpoint tests wait out moq-quic's fixed 10 s handshake
  idle timeout (30 s together). Neither moq-uring's `Transport` nor
  `--quic-idle-timeout` sets it. #6201 does not change that timeout. The
  [retirement](https://github.com/moq-dev/moq/pull/6240) settled keeping it
  separate with no new knob; do not select another API in a cost audit.
- CPU-bound simulations in a debug build: five TS export timing cases (3 to
  11 s each) and the jitter buffer's day of drift (6 to 13 s), whose `Steer`
  rebuilds the hull and median over its whole window on every step.

## Required

- [Cut and publish the prepared breaking release](/quest/m2/release/cut.md) - the release in flight ships before this run starts
