# [L] Return connection credit when the receiver releases bytes

## Goal

Native receivers can read QUIC stream bytes without returning connection
credit until their consumer releases those bytes. The same ownership works
through the tokio and io_uring adapters, including cancellation and teardown.
Stream credit remains independent, so a finite stream window does not force
an entire application message to fit before more bytes arrive.

## Plan

Decided in the 2026-10-10 receive-memory interview: use `moq-quic` after the
fork switch, not a new feature in the frozen moq-dev/noq fork. This quest
provides the transport primitive; [message assembly](/quest/m1/quic/receive-memory.md)
activates it with the recovery policy needed for progress.

Today `Chunks::finalize` returns both kinds of credit before the parser has
a complete message. Separate returning borrowed transport state from
returning connection credit. Carry ownership through the native adapters
without copying payloads solely to track credit. Prefer an owned handle or
explicit consumption accounting over callbacks; keep the surface limited to
what the message-assembly consumer needs.

Replenish `MAX_STREAM_DATA` on reads. Defer `MAX_DATA` until the owner releases
the bytes, exactly once, even after FIN, STOP_SENDING, RESET_STREAM, or stream
state reclamation. Unread discarded bytes, late data on stopped streams,
reset final-size gaps, and already-read retained bytes must not leak or
return the same credit twice. Releasing useful credit must eventually wake
a blocked peer even below the normal window-update threshold; do not depend
on optional DATA_BLOCKED frames. Runtime window shrink debt still applies.

Expose enough credit accounting and events for MoQ to detect assembly
pressure. QUIC performs cancellation but does not choose application victims.
A sender's write being blocked, or its data being acknowledged, does not
prove receiver assembly deadlock. No sender-side shedding is added.

Tests use the transport simulator and controlled time: stream credit advances
while connection credit stays held, explicit release resumes the peer, and
all terminal paths account once even when retained bytes outlive the stream.
Cover a useful release smaller than the update threshold and compose with
reliable reset when available. Wire adapter tests into CI and measure the
accounting cost across stream counts and payload sizes.

Public API: a native receive-credit ownership/observation seam, finalized
with its consumer. Wire: existing QUIC flow-control and cancellation frames;
no new MoQ message or negotiation. Browser and iroh receivers do not gain a
capability their transports cannot provide.

## Required

- [Switch MoQ onto moq-quic](/quest/m1/quic/fork/switch.md) - both native adapters use the in-tree transport

## Related

- [Bound message assembly](/quest/m1/quic/receive-memory.md) - retains connection credit until a complete message is handed off
- [Reliable stream reset](/quest/m1/quic/reliable-reset.md) - compose retained credit with reliable prefixes and final-size accounting
- [Relay peers get wider limits](/quest/m1/quic/peer-limits.md) - runtime receive-window changes preserve retained-credit accounting
