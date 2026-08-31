# [M] moq-video: capture negotiates twice, so a window resize between the probe and the first subscriber strands consumers that fixed on the first snapshot

## Goal

Implement and verify the behavior tracked in [#2799](https://github.com/moq-dev/moq/issues/2799)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

`moq_video::encode::publish_capture` negotiates its source twice: once to learn the mode (so the catalog rendition can be exact before anything is encoded), and again when a subscriber arrives and the live encoder starts. Between those two opens the source can change.

Camera capture picks a stable device mode, so the two agree in practice. macOS **window** capture does not: `screencapture.rs`'s `open_window` derives geometry from the window on every open, so a user resizing the window between the probe and the first subscriber gets a live encoder whose mode differs from the advertised one.

The catalog itself recovers, because the importer republishes the SPS-derived dimensions and codec once real frames flow. What doesn't recover is a consumer that already committed to the first snapshot. `moq_transcode::run` is the concrete case, and it says so:

```rust
// The rung set is fixed at startup: a source that changes resolution
// mid-stream keeps the ladder it started with, but the passthrough entries
// track the source.
```

So a resize between the two opens leaves the ladder sized for a picture the stream no longer carries.

This is not new. `origin/main` has the same two negotiations, just arranged differently: `catalog_ready` starts false, so the camera opens and encodes unprompted until the first keyframe (negotiation #1), then breaks on no-viewers, releases the camera, and reopens when demand arrives (negotiation #2). Its catalog dimensions come from #1 and are equally invalidated by #2. #2768 changes only how #1's geometry is learned (probe rather than encode), not that there are two.

Two directions, either of which closes it:

1. Hold the source open from the probe through to the first subscriber, so there's one negotiation. Costs a camera/window held open while idle, which is what the demand gating exists to avoid.
2. Let the transcode ladder follow a source resolution change instead of fixing it at startup. That's the more general fix, since a source can change size mid-stream for reasons that have nothing to do with capture (a reconnecting publisher, a renegotiated screen share), and today's behavior is documented but still wrong for the consumer.

Found by Codex while reviewing #2768.

## Closes

- [#2799](https://github.com/moq-dev/moq/issues/2799) - close this issue when the quest finishes
