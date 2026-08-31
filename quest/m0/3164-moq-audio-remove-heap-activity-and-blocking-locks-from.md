# [M] moq-audio: remove heap activity and blocking locks from the capture callback

## Goal

Implement and verify the behavior tracked in [#3164](https://github.com/moq-dev/moq/issues/3164)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Follow-up to #2479, found while confirming the full-duplex audio parity tracked by #2481. #2487 bounded the callback-to-encoder queue, but #2479 explicitly left the per-callback allocation for later. The capture path is bounded now, but it is not yet real-time safe in the sense the iroh-live source path was.

#### Current failure mechanism

Every cpal callback allocates a new `Vec<f32>`:

- F32 uses [`data.to_vec()`](https://github.com/moq-dev/moq/blob/85c9f489195dde93fc1d4619c7ec00864975fc3a/rs/moq-audio/src/capture.rs#L219-L224).
- I16 and U16 use [`collect()`](https://github.com/moq-dev/moq/blob/85c9f489195dde93fc1d4619c7ec00864975fc3a/rs/moq-audio/src/capture.rs#L225-L234).

When the depth-8 queue is full, `Sender::push` drops the rejected `Vec` on the same callback thread, which can run the allocator again:

- [`capture/channel.rs::Sender::push`](https://github.com/moq-dev/moq/blob/85c9f489195dde93fc1d4619c7ec00864975fc3a/rs/moq-audio/src/capture/channel.rs#L53-L65)

With AEC enabled, the callback also takes a blocking `std::sync::Mutex` in [`Canceller::process`](https://github.com/moq-dev/moq/blob/85c9f489195dde93fc1d4619c7ec00864975fc3a/rs/moq-audio/src/aec.rs#L218-L228). The playback driver takes the same state lock when it replaces the render-reference channel. Callback buffers above the pre-sized threshold can also grow AEC scratch storage on that thread.

Heap contention or lock contention can delay the cpal callback long enough to cause the underrun or overrun that these queues are meant to prevent. Bounded memory alone does not make the callback path real-time safe.

#### Required behavior

- Move microphone samples through preallocated bounded storage. A fixed-resample SPSC endpoint or a buffer pool are both plausible, as long as callback push never allocates, frees, waits, or logs.
- Convert non-f32 device formats in fixed preallocated chunks rather than collecting a new buffer.
- Preserve the current drop and gap semantics. Overflow must remain observable so the producer re-anchors its timeline.
- Make AEC callback-owned state exclusive to the callback. Device-switch updates should cross a bounded nonblocking handoff rather than contend on its state lock.
- Size scratch storage before `stream.play()`, or process oversized callbacks in bounded chunks, so callback size cannot trigger a one-time allocation.
- Keep allocations on the async consumer side acceptable. That side is not the real-time thread.

#### Tests

Factor the callback adapter so hardware-free tests can feed F32, I16, and U16 buffers through it. Assert bounded overflow and gap reporting, chunk sizes above the normal host period, and AEC reference replacement. Add an allocation-counting test or equivalent instrumentation proving the callback adapter performs no allocation or free after construction.

Refs #2479, #2481.

## Closes

- [#3164](https://github.com/moq-dev/moq/issues/3164) - close this issue when the quest finishes
