# [M] Tests can't wait on the real clock

## Goal

A new test that waits on the real clock fails every run instead of flaking
1 in 20 under load. Today's lints ban how a timer is spelled, not the clock
it runs on: `clippy.toml` and biome send every timer to `kio::clock` or
`@moq/clock`, whose `sleep`/`timeout` still wait in real time unless the
test installs a paused, simulated, or `Manual` clock. So a test passes every
lint and still proves "nothing happened" by waiting 200 ms of wall time,
which is how the `copy_fill` `idle()` flake landed
([moq-dev/moq#7845](https://github.com/moq-dev/moq/pull/7845)) three days
after the lint.

The tests already waiting in real time are burned down in m1's
[Tests run on a mocked clock](/quest/m1/mock-clock-tests/README.md).

## Plan

Decided 2026-11-06 (maintainer, /quest-complete, from an audit of every
test's clock):

- ✅ **JS fails a test when a real-clock timer fires**, with a scoped
  opt-out, plus a `REVIEW.md` rule and a biome deny on the `Bun` global. Not
  chosen: a grep check over changed lines (misses shared helpers); a review
  rule alone (reviewers flagged #4039, #3641, #1717, and #7845, and the
  waits landed anyway).
- ✅ **Rust spikes the same guard**, falling back to a grep check if the
  spike fails. Not chosen: a grep check only; JS only for now.
- ✅ m0 for the guards, since each real-time wait is a future merge-queue
  flake; the burn-down is m1.

This README's own work: restore the rule to `REVIEW.md`, for every language.
"Unit tests mock time instead of depending on wall-clock timing or sleeps"
sat in root `AGENTS.md` from #4286 until
[moq-dev/moq#7761](https://github.com/moq-dev/moq/pull/7761) moved it into
`quest context`, where online reviewers no longer see it; today only
`rs/AGENTS.md` and `rs/moq-net/AGENTS.md` say anything. The rule should say
what a fix looks like: a mocked clock or an awaited event, never a shorter
or longer real wait, and the same fix in every language's sibling test.

## Required

- [JS tests fail when a real timer fires](/quest/m0/clock-guard/js.md) - `@moq/clock` fails a test whose real-clock timer fires, with today's offenders opted out
- [Spike a Rust real-timer guard](/quest/m0/clock-guard/rust.md) - try the same guard in `kio`, or fall back to a grep check for new real-clock tests
