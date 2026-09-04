# [L] Run qmux on the QUIC stream state machine

## Goal

qmux uses noq-proto for stream lifecycle, flow
control, datagram buffering, priority scheduling, and transport parameters.
It owns only record framing and the semantics that differ from QUIC, instead
of maintaining a parallel stream implementation.

## Plan

Start from the green
[kixelated/quinn#2](https://github.com/kixelated/quinn/pull/2) prototype. Rebase
it onto noq and preserve its central invariant: drive the
existing stream state machine through the same receive and write entry points
as QUIC, then treat serialization to the reliable underlying transport as the
acknowledgment. Do not copy the stream maps, flow-control accounting, reset
state, or datagram queues into a qmux-specific implementation.

Keep the shared-core patch narrow. The prototype needs only module wiring, a
receive-offset accessor, frame-iterator access, and limited datagram helper
visibility. Review each exposure as a reusable internal boundary rather than
making the whole QUIC connection public.

`RESET_STREAM_AT` is a QUIC extension, not a qmux-specific frame. Once the
reliable-reset quest lands, make qmux drive that shared send and receive state
instead of retaining the prototype's local parsing and transitions.

Carry the hierarchical send groups from the scheduler quest into qmux's
record writer. Qmux over TCP, TLS, WebSocket, Unix sockets, and in-memory
duplex transports must produce the same subscription fairness and intra-group
ordering as raw QUIC.

Add the missing wire evidence before release: golden draft-02 vectors,
bidirectional interoperability against the published `qmux` 0.5.x crate, and
the TypeScript qmux/WebSocket peer used by `js/net`. Preserve rejection of
prohibited QUIC frames, params-first setup, record-size validation, close and
reset semantics, keep-alive behavior, and bounded flow-control tests.

Move the released implementation into `moq-dev/web-transport` as the canonical
qmux crate or make that crate a thin transport adapter over noq-proto.
There must be one stream state machine in the dependency graph.

## Required

- [Establish the noq relationship](/quest/m2/quic/parent.md) - port the
  prototype onto noq once the maintainers have seen the plan
- [Reliable stream reset](/quest/m2/quic/reliable-reset.md) - qmux reuses the
  extension's stream state rather than implementing reset locally
- [Hierarchical stream scheduling](/quest/m2/quic/scheduler.md) - qmux must
  expose the same scheduling contract as native QUIC
