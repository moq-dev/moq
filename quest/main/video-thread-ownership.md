# [M] Enforce synchronous codec thread ownership

## Goal

Public synchronous video Encoder and Decoder handles cannot be moved or dropped
on the wrong thread. Async Sink handles retain their supported worker-owned
execution model.

## Plan

The private backend traits require Send, and the Windows implementations justify
unsafe Send by confinement to a Sink worker. Public direct constructors bypass
that confinement. Media Foundation's ComGuard must balance initialization and
destruction on the thread that opened it.

Keep both synchronous and async APIs. Enforce thread confinement in the type
system instead of documentation or an unsafe Send promise. Construct, use,
and destroy the backend on its worker without requiring the codec itself to
cross threads. Preserve platform frame affinity, including macOS native frames.
Balance successful COM initialization if subsequent MF startup fails.

Add compile-time ownership coverage and a fake backend recording construction,
calls, cancellation, and destruction threads. Keep the existing rule that a
cancelled queued Sink request poisons further use; document that Consumer
reads inherit it. Wire Windows compilation into the existing platform CI lane.

Public API: synchronous codec auto-trait guarantees become stricter. Wire: none.
