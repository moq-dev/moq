# [M] Keep-alive over a moq-lite PING

## Goal

A session on lite-08-wip can keep alive over any transport, browser
WebTransport included, with the [session keep-alive](/quest/m1/keep-alive.md)
option and its `Live`/`Stale` watch. moq-lite gains a PING control message
that the peer answers, so a side with no QUIC PING of its own can still
probe a quiet session. The browser WebTransport refusal that the m1 quest
ships goes away for lite-08 sessions.

## Plan

Decided 2026-11-25 (maintainer, on
[moq-dev/moq#9666](https://github.com/moq-dev/moq/pull/9666)): ✅ a moq-lite
PING message on lite-08-wip, so keep-alive works on every transport
(rejected: qmux only, refusing it on WebTransport for good; polling
`getStats()`, which is a timer loop). It's split from the m1 quest because
lite-08-wip opens only after [the lite-07
cut](/quest/m2/lite07-finalize.md). It joins m3, beside the other lite-08
work.

- PING exists only on lite-08-wip, so published versions are unchanged. On
  lite-07 and earlier, and on moq-transport, keep-alive falls back to QUIC
  PINGs where the transport has them. Where it has none (browser
  WebTransport), the option is still refused at connect.
- Only a side that can't probe below MoQ sends PING. A native QUIC stack
  keeps using QUIC PINGs, which ride its loss recovery. Every lite-08
  endpoint and relay answers a PING, which is what lets a browser use it.
  The answer is a reply under the m1 quest's rule (any packet counts), so
  `Stale` and `hard` work the same as they do over QUIC.
- Bound what a peer can make us do: answer PINGs at most at the rate the
  peer's own `soft` would justify, and coalesce a burst into one answer.
  Never queue one answer per PING.
- Update `drafts/draft-lcurley-moq-lite.md` in the same PR, and change Rust
  (`moq-net`) and JS (`@moq/net`) together. Run `just test interop --all`.

Test it the way the m1 quest is tested, on the sim with bufferbloat, with a
JS endpoint whose transport has no QUIC PING. Add a lite-07 peer to check
that the fallback refuses rather than goes quiet.

## Required

- [The lite-07 cut](/quest/m2/lite07-finalize.md) - lite-08-wip, which carries PING, opens after it
- [Session keep-alive](/quest/m1/keep-alive.md) - the option and watch this carries over a new transport

## Related

- [Metadata mode on the lite-08 wire](/quest/m3/route-health/wire.md) - other lite-08-wip work
- [A preempted group pauses and continues in lite-08](/quest/m3/group-takeover.md) - whichever starts first opens lite-08-wip
