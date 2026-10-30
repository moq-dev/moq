# [L] Padding probes from the browser

## Goal

A browser session (`js/net` over WebTransport) on lite-07 probes up to its
target with padding datagrams sent below every media stream, so padding never delays
media at the sender and is dropped rather than queued when there is no room.
A shaped link in Chrome proves it: with padding the estimate rises past the
media, without it it stays frozen, and on a path with no room padding stays
within its overhead and media frame-delay budgets. Such lite-07 sessions
then have a sender, so
[viewer targets](/quest/m2/probe/viewer-target.md) can advertise PROBE
`Increase` for them.

## Plan

[moq-dev/moq#7390](https://github.com/moq-dev/moq/pull/7390) built a sender
on padding **streams** and is on hold as a draft. Its send target
(`Allocator.setTarget`, `Reservation.probed`, `reload.ts` following the
target into the session) and its GoogCC ALR schedule are reusable; the
writer is not.

Decided with the maintainer on 2026-10-30: probing has to send above the
estimate about 1/8 of the time (BBR's ProbeBW gain cycle), and bursts that
often cause the congestion they probe for. A padding stream at the lowest
`sendOrder` still holds its bytes in the send buffer and at the bottleneck
once written, so a frame ready just after a cluster waits behind it. Padding
should instead use datagrams at the lowest priority: unreliable, never
retransmitted, and dropped by the sender once stale.

✅ Decided with the maintainer on 2026-10-30: padding datagrams are
supported **only on lite-07**. Neither IETF moq-transport nor lite-05/06
gets a sender, and #7390's moq-transport PADDING stream writer goes with the
rest of the stream writer. lite-07 gains a padding datagram type that
receivers discard before its cut. That type is its own quest, gating
[Finalize moq-lite-07](/quest/m1/lite07-finalize.md)
([moq-dev/moq#7419](https://github.com/moq-dev/moq/pull/7419)).

What the browsers allow, checked on 2026-10-30:

- **Spec.** The [WebTransport spec](https://w3c.github.io/webtransport/)
  now gives datagrams per-writable priority:
  `transport.datagrams.createWritable({ sendGroup, sendOrder })` returns a
  `WebTransportDatagramsWritable`, and its send-order rules rank streams
  and datagrams together within a send group (sending "MUST starve until all
  bytes queued for sending on streams and datagrams with the same
  [[SendGroup]] and a higher [[SendOrder]] ... have been sent"; the default
  null group is a group). So a datagram writable at
  `Number.MIN_SAFE_INTEGER` in the default group, with every media stream in
  that group, is sent only when no stream has bytes ready. The older rule
  ([w3c/webtransport#451](https://github.com/w3c/webtransport/issues/451),
  closed by [w3c/webtransport#604](https://github.com/w3c/webtransport/pull/604))
  said datagrams SHOULD go ahead of streams without starving them; per-writable
  `sendOrder` is how a page asks for the opposite.
- **Firefox** ships `createWritable` with `sendOrder` and `sendGroup` from
  Firefox 155, and **Safari** from 26.4 (MDN browser-compat-data). Neither
  is verified to rank a datagram writable against streams yet; Firefox
  already sent datagrams last by default (WebTransport WG, April 2024, in
  #451).
- **Chrome** does not ship `createWritable` (MDN browser-compat-data: no
  version). Chromium's implementation sits behind an experimental runtime
  flag and stores `sendOrder` without passing it to the network service.
  Through the legacy `datagrams.writable` Chrome sends datagrams ahead of
  every stream, so padding datagrams there would preempt media, the opposite
  of what we want.

So, to do:

- **Wire.** The lite-07 padding datagram type lands before the cut, under
  [Finalize moq-lite-07](/quest/m1/lite07-finalize.md). The sender uses
  it only on lite-07. If `drafts/draft-lcurley-moq-probe.md` names
  moq-transport padding as a mechanism, scope it to lite-07.
- **Sender.** On lite-07 only, replace #7390's `Padding` writer with one datagram writable
  from `createWritable({ sendOrder: Number.MIN_SAFE_INTEGER })`, with a short
  `outgoingMaxAge` so queued padding expires instead of sending late. Keep
  the send target and ALR schedule, retuned for datagrams.
- **Chrome.** Until Chrome honors datagram `sendOrder`, either send no
  padding there (feature-detect `createWritable` and check it ranks below
  streams) or find another way to keep padding behind media. Track the
  Chromium work; this decides whether the quest is useful in Chrome at all.
- **Browser test.** Through a shaped link: with padding the estimate rises
  past the media, without it it stays frozen. On a path held below the
  target, report media frame delay and deadline misses against budgets set
  before tuning, and assert that repeated clusters on a path with no room
  stay within the overhead budget. Wire it into CI at least nightly, sharing
  the epic's shaped-uplink setup (the nightly already plays the matrix in
  headless Chromium through moq-shaper). Check what the estimate actually
  does under padding: a Cubic estimate (cwnd over RTT) grows slowly during a
  short cluster, and a BBR estimate aged down to the media keeps the session
  from reading as app-limited at all; either may call for retuning.
