# [L] Padding probes from the browser

## Goal

A browser session (`js/net` over WebTransport) can't reach its congestion
controller, so it probes up to its target with a padding stream at the lowest
send priority. On an ALR schedule, while app-limited below the target, it
sends a short probe cluster at about twice the estimate, capped at the target,
about every five seconds, with a follow-up probe when one validates. When the
path has room the estimate rises; when it doesn't, padding stays within its
overhead and media frame-delay budgets. Such sessions then advertise PROBE
`Increase`.

## Plan

Decided with the maintainer on 2026-10-17 (see the
[epic](/quest/m2/probe/README.md)):

- **Padding is the browser's sender.** Native sessions retransmit in-flight
  data instead ([native probing](/quest/m1/probe-native.md)). Padding is
  ordinary stream data that any controller counts and paces, and it
  implements the [target](/quest/m1/probe-target.md) quest's sender hook in
  `js/net`. Decided 2026-10-20: browser-only. moq-quic is the only Rust
  transport, so no native session needs it.
- **WebRTC's schedule.** Model it on GoogCC's `ProbeController` ALR probing.
  Rejected: [#5863](https://github.com/moq-dev/moq/pull/5863)'s exponential
  backoff to 60 s, which leaves a rendition waiting up to a minute after the
  path opens. A cluster that finds congestion (a falling estimate) ends
  early.
- **Wire.** lite-07 reserves the padding stream type
  ([lite-07 padding](/quest/m1/lite07-padding.md)), matching moq-transport's
  PADDING. A moq-lite peer that predates it resets the unknown stream without
  closing the session, so the probe just ends. On moq-transport, send PADDING
  only from draft-18 on: drafts 14 to 17 close the session on an unknown
  stream type (`UniType::classify`), so refuse to pad there and keep
  `Report`. Update `drafts/draft-lcurley-moq-probe.md` if the sender changes
  what it says, and run `just drafts check`.
- A browser sender can't see whether it is app-limited; settle how to judge
  it from what it sends against the estimate.
- Settle how the schedule stays within its overhead budget: how padding's
  share of sent bytes is counted over time, and what a cluster does once it
  would exceed the budget.

Tests, on `@moq/clock`'s `Manual`:

- The schedule: nothing while media meets the target or the sender isn't
  app-limited; a cluster about every five seconds at twice the estimate,
  capped at the target; a follow-up after one that validates; an early end
  when the estimate falls.
- The padding writer over a fake WebTransport: padding goes out at the lowest
  priority on lite-07 and IETF draft-18, an old lite peer resets it and the
  session stays up, and drafts 14 to 17 pad nothing.
- In Chrome through a shaped link: with padding the estimate rises past the
  media, without it it stays frozen. On a path held below the target, report
  media frame delay and deadline misses against budgets set before tuning:
  lowest priority can't preempt padding already in flight or queued at the
  bottleneck, so a frame ready just after a cluster can still wait behind it.
  Assert that repeated clusters on a path with no room stay within the
  overhead budget.
  Wire it into CI at least nightly, sharing the epic's shaped-uplink setup.

## Required

- [Session probe target](/quest/m1/probe-target.md) - the target this probes to and the sender hook it implements
- [lite-07 padding](/quest/m1/lite07-padding.md) - the stream type the padding is sent on
