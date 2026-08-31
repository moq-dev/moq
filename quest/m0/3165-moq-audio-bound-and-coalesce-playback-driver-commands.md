# [M] moq-audio: bound and coalesce playback driver commands

## Goal

Implement and verify the behavior tracked in [#3165](https://github.com/moq-dev/moq/issues/3165)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Part of the full-duplex audio parity audit from #2481. #2478 called for a bounded driver command channel, matching iroh-live's bounded device-control path. The landed playback mixer and sample rings are bounded, but the device driver's outer command queue is not.

#### Current failure mechanism

[`Engine::open`](https://github.com/moq-dev/moq/blob/85c9f489195dde93fc1d4619c7ec00864975fc3a/rs/moq-audio/src/playback.rs#L98-L126) creates the driver channel with `std::sync::mpsc::channel()`. This queue is unbounded. Clones of its sender are used for:

- output-device switches,
- sink and AEC synchronization wakes,
- shutdown,
- cpal error notifications from every stream generation.

The driver performs blocking host operations while opening or switching devices. During one of those stalls, repeated cpal errors or caller commands can continue allocating queue nodes without a memory bound. The cpal error callback also uses the allocating `send` path before returning.

The inner mixer queue is correctly bounded and retried, so the unbounded outer queue is now the only control-plane exception in the playback engine.

#### Required behavior

- Give the driver command path a fixed memory bound and never block a cpal callback on capacity.
- Coalesce idempotent `Sync` wakes instead of queueing one per event.
- Coalesce or otherwise bound failure notifications by live stream generation. A stale generation must not evict the signal for the current stream.
- Preserve reliable switch completion. A switch request must either enter the bounded driver path or return a clear overload or stopped error to its awaiting caller.
- Preserve reliable last-handle shutdown without requiring an unbounded emergency lane.
- Keep the existing mixer retry invariant. A dropped wake must not leave a sink or AEC tap permanently unattached.

#### Tests

- Flood each notification class while the driver receiver is paused and assert storage never exceeds the fixed bound.
- Confirm a saturated path cannot strand an awaiting switch response.
- Confirm a live-generation failure still schedules restart when stale failures and sync wakes are already pending.
- Confirm dropping the final engine or sink shuts the driver down under saturation.

Refs #2478, #2481.

## Closes

- [#3165](https://github.com/moq-dev/moq/issues/3165) - close this issue when the quest finishes
