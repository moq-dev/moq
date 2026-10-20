# [S] Resume through an undetected outage

## Goal

A same-epoch resume after a route dies without a close (a killed relay,
detected only at the QUIC idle timeout) delivers every group the outage
backlog still holds within the subscriber's max age, from the first frame the
reader lacks. A regression test pins this, and the docs stop promising more
than max age allows.

## Plan

Reported in [#6062](https://github.com/moq-dev/moq/issues/6062): a diamond
O -> M1/M2 -> E on lite-07 with `--quic-idle-timeout 6s`, M1 SIGKILLed, and
`moq export ts` on E. Each exporter logged `media skipped upstream` for
2-7 s on every track, at both the default `--delay` and `--delay 10s`.

Triage against `5284d969bf` (2026-10-20, code reading only):

- Resume already asks for the right floor. `Reader::sync` subscribes the new
  copy at `floor = self.newest` (`rs/moq-net/src/model/resume.rs`), and
  `Subscription::clamped` raises the wire start to it. A cold M2 forwards that
  start to O.
- Every hop then applies the subscriber's max age, which is `--delay / 2` for
  `export ts`. Under it, groups that went stale during detection are expired,
  as the draft requires ("A floor is not a request", `Group Start` in
  `drafts/draft-lcurley-moq-lite.md`). At the default 1 s delay, losing most
  of the 6.6 s window is intended.
- At `--delay 10s` (a 5 s budget) the code predicts about 2 s lost, not the
  same loss as the default. Nothing read explains the report there, so it may
  be a real bug. Candidates: the wall-clock `superseded` rule in
  `model/track.rs`; `sync` finding no copies and dropping the floor
  (`resume.rs`, `subscription.rs` `clamped`); or the container consumer's
  hole and latency skips (`rs/moq-mux/src/container/consumer.rs`).
- [#6019](https://github.com/moq-dev/moq/pull/6019) and
  [#6024](https://github.com/moq-dev/moq/pull/6024) do not touch this path.

Decided with the maintainer, 2026-10-20:

- Keep max-age semantics. Recovering the whole gap regardless of max age
  (widening max age on resume, or fetching the gap) was declined.
- Test at the model level only, in
  `rs/moq-net/tests/integration/route_change.rs` under mock time, next to
  `lite_resumes_after_a_lagging_route_dies`. The alternate route starts cold.
  The test writes timestamped groups while A's route is silent but not yet
  closed, then aborts it. With a budget above the outage, every group
  arrives. With a budget below it, only the in-budget tail arrives. No
  end-to-end relay kill test.
- The `--delay 10s` loss must be explained before this closes #6062. If the
  moq-net test passes, add a mock-time unit test in
  `rs/moq-mux/src/container/consumer.rs` that feeds the consumer a resumed
  backlog burst within its budget, and assert it reads every group without a
  `Read::Gap`.
- Fix any bug the test exposes at its source in moq-net, not in the exporter.
- Correct `doc/concept/moq-lite.md` (*Same epoch resumes*): the resume point
  is a floor, and the subscriber's max age still bounds the backlog.
- Faster detection of the dead route belongs to
  [route health](/quest/m2/route-health/README.md), not here.

## Closes

- [#6062](https://github.com/moq-dev/moq/issues/6062) - same-epoch resume after a relay loss continues at a later group

## Related

- [Resumed in-flight groups](/quest/m1/resume-in-flight.md) - which replaced copies stay while groups below them are in flight
- [A widened max age re-offers cached groups](/quest/m1/resolved-demand.md) - the same max-age bound on a warm relay
- [Redundant failover drift](/quest/m1/test-flakes-2/redundant-failover-drift.md) - the same idle-timeout outage hitting the TS jitter clock
