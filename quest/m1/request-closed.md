# [M] A held request knows when nobody waits

## Goal

A claimant holding a `Request` learns, through the request itself, when no
requester waits on its answer any more: each one left, or the relay moved it
to another route. Rust's `Request` gains `closed()` (and `poll_closed`), and
JS's `Request` (`js/net/src/origin.ts`) the mirrored `closed()`, so a
claimant skips or stops work no one will read.

Non-goals: any holding policy, launching, or autoscaling, which stay with
the claimant's owner.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan, from kixel.ai's
[capabilities epic](https://github.com/kixel-ai/kixel.ai/blob/main/quest/m2/capabilities/README.md)):
✅ a generic claim primitive in moq, m1 as additive API (rejected: building
it in Kixel, which would fork moq-net's private request state; not now).
Kixel's orchestrator holds a request while a node boots and relays move it to
the node's cheaper claim (`rs/moq-net/src/model/front.rs`), but the holder is
never told, so it may launch nodes no one needs. moq.pro's wildcard transcode
workers have the same waste when a viewer leaves before a job starts.

Guidance:

- The result channel is already a `kio::Producer<PendingBroadcast>`, whose
  `poll_closed` fires when every consumer is gone. Check that each way a
  requester stops waiting drops its consumer: a local `Consumer::request`
  future dropped, a remote SUBSCRIBE cancelled before its answer
  (`rs/moq-net/src/lite/publisher.rs` blocks on `poll_ok`), and a front
  re-routing to a better route. Fix whichever holds it, rather than adding a
  second signal.
- Coalesced requesters share one request, so it closes only when the last
  leaves.

Tests, with no sleeps: each of the three ways closes a held request; one of
two coalesced requesters leaving does not; the same in JS.
