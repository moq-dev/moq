# [L] Padding probes from the browser

## Goal

A browser session (`js/net` over WebTransport) can't reach its congestion
controller, so it probes up to its target with a padding stream at the lowest
send priority. On an ALR schedule, while app-limited below the target, it
sends a short probe cluster at about twice the estimate, capped at the target,
about every five seconds, with a follow-up probe when one validates. When the
path has room the estimate rises; when it doesn't, padding stays within its
overhead and media frame-delay budgets. Such sessions on lite-07 or IETF
draft-18 and later then have a sender, so
[viewer targets](/quest/m2/probe/viewer-target.md) can advertise PROBE
`Increase` for them.

## Plan

Decided with the maintainer on 2026-10-17 (see the
[epic](/quest/m2/probe/README.md)):

- **Padding is the browser's sender.** Native sessions retransmit in-flight
  data instead (native probing, [moq-dev/moq#6635](https://github.com/moq-dev/moq/pull/6635)). Padding is
  ordinary stream data that any controller counts and paces, and it
  implements the sender hook in `js/net`, mirroring Rust's
  `transport::Probe`. Decided 2026-10-20: browser-only. A native session without
  native probing (iroh on noq, or moq-quic under CUBIC) has no sender, so
  [native rung disable](/quest/m1/simulcast-rung-disable.md) never stops a
  rung there.
- **The browser's send target.** The `js/net` mirror of the Rust send
  target (`Allocator::set_target`, `Session::bandwidth`, `transport::Probe`) lands here, beside the sender that
  uses it: the same reservation sum, opt-out, and sender seam, matching the
  Rust API. `js/net` already owns its connection's `Allocator`
  (`js/net/src/connection/reload.ts`). Moved here in review of
  [#6102](https://github.com/moq-dev/moq/pull/6102), so m1 ships no browser
  target without a sender.
- **WebRTC's schedule.** Model it on GoogCC's `ProbeController` ALR probing.
  Rejected: [#5863](https://github.com/moq-dev/moq/pull/5863)'s exponential
  backoff to 60 s, which leaves a rendition waiting up to a minute after the
  path opens. A cluster that finds congestion (a falling estimate) ends
  early.
- **Wire.** lite-07 reserves the padding stream type
  (Padding Stream 0x2), matching moq-transport's
  PADDING. Pad only on lite-07 and, on moq-transport, from draft-18 on. On
  lite-05 and lite-06 the peer resets every padding stream, so the estimate
  can never rise. Those sessions have no sender: refuse to pad
  and keep `Report`. Update `drafts/draft-lcurley-moq-probe.md` if the sender changes
  what it says, and run `just drafts check`.
- A browser sender can't see whether it is app-limited; settle how to judge
  it from what it sends against the estimate.
- Settle how the schedule stays within its overhead budget: how padding's
  share of sent bytes is counted over time, and what a cluster does once it
  would exceed the budget.

Tests, on `@moq/clock`'s `Manual`:

- The send target: it follows reservations being added, updated, and
  dropped, demanded or not, reaches the padding sender, and opting out pads
  nothing, as in the Rust tests.
- The schedule: nothing while media meets the target or the sender isn't
  app-limited; a cluster about every five seconds at twice the estimate,
  capped at the target; a follow-up after one that validates; an early end
  when the estimate falls.
- The padding writer over a fake WebTransport: padding goes out at the lowest
  priority on lite-07 and IETF draft-18, while lite-05, lite-06, and drafts
  14 to 17 pad nothing and negotiate `Report`.
- In Chrome through a shaped link: with padding the estimate rises past the
  media, without it it stays frozen. On a path held below the target, report
  media frame delay and deadline misses against budgets set before tuning:
  lowest priority can't preempt padding already in flight or queued at the
  bottleneck, so a frame ready just after a cluster can still wait behind it.
  Assert that repeated clusters on a path with no room stay within the
  overhead budget.
  Wire it into CI at least nightly, sharing the epic's shaped-uplink setup.
