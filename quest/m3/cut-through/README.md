# Cut-through group streams

## Goal

A relay forwards group stream bytes that arrive past a QUIC stream hole
instead of holding them until the retransmission fills it. Payload after a
lost packet reaches the next hop at its own output offset while the hole is
still being recovered, so a loss on one hop no longer delays everything behind
it on the next.

The line measures whether this is worth it. Only the loss-delay metric and
the bench are quests today; on a go, the build quests (offset writes in
`moq-quic`, frame ranges, the transport trait, relay cut-through) are
re-planned from git history against the bench's numbers.

Non-goals: end consumers (players, `moq-cli`) keep ordered reads, since a
decoder needs whole frames. No MoQ wire change; QUIC already allows a sender to
send stream offsets in any order.

## Plan

Decided 2026-10-02: the line moved from m2 to m3. It is a speculative
optimization with no consumer asking for it, so it parks until one does and
is deleted if it goes stale. The QUIC primitives land in `moq-quic`, the
[hard fork](/quest/m1/quic/fork/README.md)'s stack.

Decided 2026-10-08: the four build quests were deleted. They churned with
every frame-model and transport change while waiting on a verdict nobody had
measured, so they are re-planned from git history if the bench says go. The
design decisions below stay as the starting point for that re-plan.

Decided 2026-09-30:

- Measure first; the [bench](/quest/m3/cut-through/bench.md) gates any build.
- The gain is roughly the time the relay would spend bursting the held bytes
  downstream after the hole fills: small when the egress hop has headroom,
  larger for a big I-frame on a tight hop. The bench sweeps exactly that.
- A hole that covers a frame header blocks every later frame on that stream:
  the next header's input position and every output header's length depend on
  earlier header values (lite's timestamp delta, IETF's object id delta). A hole
  inside a payload blocks nothing, since the frame sizes locate both the input
  and the output bytes. Passing a hole into later frames needs several
  in-flight frames per group; the build prototypes both scopes and keeps
  the one the bench justifies.
- The opportunity is measured on the real two-hop path, not against a direct
  connection: the gain exists only with a relay in the middle.
- The frame model gains an offset write and a range read, crate-private in
  `moq-net`, since the lite and IETF publishers and subscribers that use them
  live in the same crate. Cut-through frames keep the received chunks by
  offset instead of copying into a pre-allocated buffer, so memory tracks bytes
  received and the per-session pre-allocation `Budget` (`model/frame.rs`)
  does not apply. The group cache already charges each frame by bytes written
  (#4609), so several in-flight frames charge only what has arrived.
- moq-net's `transport::poll` traits gain an unordered chunk read and an
  offset write whose defaults report unsupported, and moq-net falls back to
  ordered I/O. Only the
  `moq-quic` backends implement them; browsers, qmux, and iroh keep the defaults.
  Additive, so it lands on `main`.
- Always on wherever both of the relay's streams support it. No config knob.
- The relay's own egress is `moq-quic` for every native and browser viewer, so browser
  viewers benefit too; only the relay's side needs the feature.

Public API: additive `transport::poll` methods in moq-net; the loss-delay counter on
`moq-stats` ingress rows. Wire: none for MoQ; the counter is a `moq-stats`
field.

End to end: once a relay build lands, re-run the
bench on the same sweep and record before and after in this README before
closing the line. No new doc page: nothing user-facing changes beyond the
stats field, which its quest documents inline.

## Required

- [Loss delay](/quest/m3/cut-through/loss-delay.md) - relays report, per broadcast, the ingress bytes a loss held back by at least one RTT
- [Bench](/quest/m3/cut-through/bench.md) - a lossy relay hop swept over loss, frame size, and egress headroom, measuring the post-hole drain time, with a go or no-go verdict

## Related

- [Hierarchical stream scheduling](/quest/m1/quic/scheduler.md) - orders streams; offset writes order ranges within one
- [QoS](/quest/m1/qos/README.md) - the loss-delay counter follows its per-broadcast ingress row conventions
- [Frame slots are cached for free](/quest/m1/frame-slot-charge.md) - changes how the cache charges the in-flight frames a cut-through build would hold
