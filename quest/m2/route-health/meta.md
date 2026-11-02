# [L] Metadata-only subscriptions

## Goal

Rust and JS can subscribe to group/frame progression without receiving media
payload, through the shared subscription model and whichever lite wip is current when it lands. Relay
chains forward and aggregate that demand without secretly pulling full media.

## Plan

Decided 2026-10-13: a `meta: true` field on `Subscription`, defaulting to
normal payload delivery, carried by the existing SUBSCRIBE (confirmed
2026-11-02: "SUBSCRIBE_META" was shorthand, not a new message). It must be a
live stream: `largest()` on a payload-less standby is only the SUBSCRIBE_OK
snapshot, and a one-shot TRACK_STATUS on announcement races the active route.
It does not gate lite-07. Preserve normal group/frame progression with
payload omitted. Mirror the public field and semantics across Rust, JS, and
any bindings exposing Subscription.

Framing, decided 2026-11-02: each group stream marks its metadata mode in
its own header, so a stream opened before a mode update is read under the
mode it was opened in. Keeping the mode only on the subscription was
rejected: the receiver would race updates against already-open streams.
Metadata FRAMEs keep the timestamp and declared size and carry no bytes, so
a real empty frame stays distinguishable from omitted payload. Promotion to
payload is a SUBSCRIBE update on the same subscription, re-requesting bytes
from the reader's first missing frame, including positions it observed as
metadata. Metadata-only state never satisfies a media reader, fills a cache
as a complete frame, or advances a resume cursor.

Aggregation, settled 2026-11-02: a relay pulls payload upstream only for
payload demand, and serves metadata from a payload copy it already holds.
Only payload demand creates a health standby, so metadata observers never
branch further. Each downstream metadata subscriber still receives no
payload. When only metadata demand remains, release full upstream demand.
Remove all observation demand when the last reader leaves. Preserve ranges,
priority, drop/end semantics, and retention without one global scan per
update.

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
Wire: a metadata delivery mode in the current lite wip: the SUBSCRIBE field,
a mode marker in each group stream header, and payload-less metadata FRAMEs.

## Required

- [Subscribe ranges](/quest/m1/subscribe-ranges/README.md) - final subscription range shape this extends
