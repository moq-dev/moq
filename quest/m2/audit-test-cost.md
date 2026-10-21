# [M] Test cost audit

## Goal

The slowest, flakiest, and least valuable tests are sped up, fixed, deleted,
or planned, so the suite costs what it earns. Workflows and jobs belong to the
[CI workflow audit](/quest/m1/audit-ci.md).

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

What the m1 run (2026-10-21) fixed: moq-auth's stalled re-check close on the
paused clock (11 s to 0.03 s) and its refusal re-checks by a nudge instead
of the cadence (3 s and 1 s), the 900 s TS export past-limit failure the
jitter buffer's tests already cover (16.6 s), the close-code tests dialing
`localhost` against an IPv4-only server (3 s to 0.5 s each on the NixOS
runner, which lists `::1` first), and the split-accept driver tests' 333 ms
initial RTT (3 s to 0.2 s each). A benchmark-only edit now selects only its
own Rust crate or JS workspace; a moq-stats bench edit had run the relay and
CLI suites. To catch the `localhost` race on a host without it, run the
suite under an `/etc/hosts` that lists `::1` first (`unshare -rm`, then a
bind mount over `/etc/hosts`); only the close-code tests regressed.

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
  Oversubscribing the queue job is a maintainer decision, since it loads
  every timing-sensitive test and a flake ejects a queue entry. Only the
  known redundant-importer flake failed, once, at 16 threads.
- moq-gst: about ten tests per run each pay 3 s, at random. `stop` closes the
  client, which waits out the draining period (three PTOs at the 333 ms
  initial RTT) of a dial to the tests' unreachable relay that never got an
  answer. A user stopping a pipeline whose relay is down waits the same. Ask
  whether moq-quic may skip draining for a connection that never heard from
  its peer.
- moq-uring: three endpoint tests wait out moq-quic's fixed 10 s handshake
  idle timeout (30 s together). Neither moq-uring's `Transport` nor
  `--quic-idle-timeout` sets it; ask whether the configured idle timeout
  should bound the handshake too (public API).
- CPU-bound simulations in a debug build: five TS export timing cases (3 to
  11 s each) and the jitter buffer's day of drift (6 to 13 s), whose `Steer`
  rebuilds the hull and median over its whole window on every step.
- moq-cli's export tests pace a real clip through separate processes; the
  redundant-importer cases wait out a killed importer's idle timeout
  ([moq-dev/moq#6113](https://github.com/moq-dev/moq/pull/6113) shortens it).
  moq-relay's `cluster_idle` and flapping-peer drill wait the default 10 s
  idle timeout on purpose, and moq-gst's `relay_loss` test the retry budget.
- moq-c tests took 0.8 to 1.7 s in the queue against 0.02 s locally; not yet
  explained.

## Required

- [Cut and publish the prepared breaking release](/quest/m2/release/cut.md) - the release in flight ships before this run starts
