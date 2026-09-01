# [S] moq-uring: remove SQ indirection and per-enter ring fd lookup

## Goal

Implement and verify the behavior tracked in [#3199](https://github.com/moq-dev/moq/issues/3199)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Follow-up to #2875.

`Worker::new` enables `SINGLE_ISSUER`, `DEFER_TASKRUN`, and `COOP_TASKRUN`, but the ring still has an SQ array and each `io_uring_enter` resolves the normal ring fd. Both costs are avoidable below the existing Linux 6.12 floor.

#### Mechanism

- `IORING_SETUP_NO_SQARRAY` makes SQ indexes map directly to SQEs, removing the redundant SQ-array write and lookup. The worker is a single issuer and submits in order, which is the intended shape.
- `IORING_REGISTER_RING_FDS` lets subsequent enters use the registered-ring path, avoiding normal fd lookup and reference traffic on every enter.

#### Proposal

- Add `setup_no_sqarray()` when constructing the worker ring.
- Register the ring fd immediately after construction with `register_ring_fd()`.
- Treat failure as unsupported startup, consistent with the deliberate kernel-feature floor.
- Add a kernel-gated construction test so either optimization cannot silently disappear.

#### Acceptance

Run the existing io-uring echo and relay workloads before and after. Record relay CPU, cycles, cache misses, `io_uring_enter` calls, and throughput at fixed load. Keep each optimization independently ablatable and retain it only if the measured result is neutral or positive.

## Closes

- [#3199](https://github.com/moq-dev/moq/issues/3199) - close this issue when the quest finishes
