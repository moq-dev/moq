# [L] Metadata-only subscriptions

## Goal

Rust and JS can subscribe to group/frame progression without receiving media
payload, through the shared subscription model and whichever lite wip is current when it lands. Relay
chains forward and aggregate that demand without secretly pulling full media.

## Plan

Decided 2026-10-13: prefer a `meta: true` field on `Subscription`, defaulting
to normal payload delivery. It must be a live stream: `largest()` on a
payload-less standby is only the SUBSCRIBE_OK snapshot, and a one-shot
TRACK_STATUS on announcement races the active route. It does not gate lite-07. Preserve normal group/frame progression with
payload omitted. Follow the proven mode/framing contract from Health design,
including updates on an existing subscription. Mirror the public field and
semantics across Rust, JS, and any bindings exposing Subscription.

An explicit metadata mode distinguishes omitted payload from real empty
frames. Metadata-only state cannot satisfy media readers, masquerade as a
complete cached frame, or advance their resume cursor. A mode change to media
must request actual bytes at the required position, including previously
observed metadata positions. Handle updates and already-open group streams
without interpreting old-mode frames under the new mode.

Aggregate full and metadata readers separately: real payload demand wins
only where needed, and each downstream metadata subscriber still receives
no payload. When only metadata demand remains, release full upstream demand.
Remove all observation demand when the last reader leaves. Preserve ranges,
priority, drop/end semantics, and retention without one global scan per update.

Reuse the live-largest model where its semantics fit, keeping remote reports
separate from useful received media. Standby health uses this feature only
for matching explicit epochs; the field itself does not authorize resume.
Older negotiated protocols must not silently emulate metadata-only mode by
fetching and discarding media. Explicitly report unsupported mode; ordinary
media and epochless Restart continue unchanged. An IETF counterpart is not
required for this lite feature.

Tests: metadata-only multi-hop traffic has no media payload; mixed demand;
promotion/demotion during an open group; empty frames; late old-mode frames;
resume at a metadata-observed position; cancellation; refusal on unsupported
versions. Sweep tracks, readers, and relay depth for bytes, wakes and memory.
Update `draft-lcurley-moq-lite.md`, Rust/JS codecs, and stale API docs together.
Run `just check`, `just drafts check`, and `just test interop --all`; add the
new mode to cross-language interop and existing CI.

Public API: `Subscription.meta` and any corresponding binding surface.
Wire: a metadata delivery mode in the current lite wip; exact encoding is settled by Health design.

## Required

- [Health design](/quest/m2/route-health/design.md) - progress and mode-transition contract
- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - final subscription range shape this extends
