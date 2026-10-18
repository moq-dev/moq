# [L] Padding probes where the stack can't retransmit

## Goal

Where a session's transport has no native probe (a browser over WebTransport
through `js/net`, a moq-net session on another QUIC backend), the session
probes up to its target with a padding stream at the lowest send priority. On
an ALR schedule, while app-limited below the target, it sends a short probe
cluster at about twice the estimate, capped at the target, about every five
seconds, with a follow-up probe when one validates. When the path has room
the estimate rises; when it doesn't, padding stays within its overhead and
media frame-delay budgets. Such sessions then advertise PROBE `Increase`.

## Plan

Decided with the maintainer on 2026-10-17 (see the
[epic](/quest/m2/probe/README.md)):

- **Padding is the fallback, not the first choice.** A browser can't reach
  its congestion controller, and other backends lack
  [native probing](/quest/m2/probe/native.md), so padding is ordinary stream
  data that any controller counts and paces. It implements the
  [target](/quest/m2/probe/target.md) quest's sender hook wherever the
  transport doesn't.
- **WebRTC's schedule.** Model it on GoogCC's `ProbeController` ALR probing.
  Rejected: [#5863](https://github.com/moq-dev/moq/pull/5863)'s exponential
  backoff to 60 s, which leaves a rendition waiting up to a minute after the
  path opens. A cluster that finds congestion (a falling estimate) ends
  early.
- **Wire: a moq-lite padding stream** (kept from #5863's analysis). Add a
  unidirectional stream type the receiver reads and discards, matching
  moq-transport's PADDING, which moq-net already discards
  (`rs/moq-net/src/ietf/session.rs`). A moq-lite peer that predates it resets
  the unknown stream without closing the session, so the probe just ends.
  On moq-transport, send PADDING only from draft-18 on: drafts 14 to 17 close
  the session on an unknown stream type (`UniType::classify`), so refuse to
  pad there and keep `Report`. Update `drafts/draft-lcurley-moq-lite.md` and
  `drafts/draft-lcurley-moq-probe.md` in this PR and run `just drafts check`.
- A browser sender can't see whether it is app-limited; settle how to judge
  it from what it sends against the estimate.

Tests, with mocked time:

- The schedule, Rust and JS: nothing while media meets the target or the
  sender isn't app-limited; a cluster about every five seconds at twice the
  estimate, capped at the target; a follow-up after one that validates; an
  early end when the estimate falls.
- moq-net under its simulated transport: padding goes out at the lowest
  priority; lite and IETF draft-18 receivers discard it; an old lite peer
  resets it and the session stays up; drafts 14 to 17 pad nothing.
- `js/net`: the padding writer over a fake WebTransport under `@moq/clock`'s
  `Manual`.
- moq-quic's seeded simulation with a stepped bottleneck and native probing
  off: with padding the estimate rises past the media, without it it stays
  frozen. On a path held below the target, report media frame delay and
  deadline misses against budgets set before tuning: lowest priority can't
  preempt padding already in flight or queued at the bottleneck, so a frame
  ready just after a cluster can still wait behind it. Wire it into CI at
  least nightly.

## Required

- [Session probe target](/quest/m2/probe/target.md) - the target this probes to and the sender hook it implements
