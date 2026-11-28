# [XS] Bound moq-gst drained() checks after track cancellation

## Goal

After cancellation wins in `moq-gst`'s source pad loop, `drained()` checks whether the cursor already reached a clean end without running an unbounded synchronous loop that polls arbitrary new groups from a hostile or broken publisher.

## Plan

Decided 2026-11-28 (maintainer, `/quest-plan` of the pre-0.22 design-gate audit): ✅ fix in `m3` (disagreeing with the `--accept` rationale on [moq-dev/moq#9695](https://github.com/moq-dev/moq/pull/9695)).

Caused by [moq-dev/moq#9695](https://github.com/moq-dev/moq/pull/9695).

In `rs/moq-gst/src/source/imp.rs:1897-1921`, `drained()` loops synchronously after cancellation, calling `moq_mux::container::Consumer::poll_read` (`rs/moq-mux/src/container/consumer.rs:691-705,971-975`), which skips `Item::GroupEnd` via `continue` while `pending.pop_front()` refunds buffer credit. A publisher streaming empty groups can stall pad handoff to a replacement track indefinitely.

Fix: add a read-only clean-end check on `moq_mux::container::Consumer` (or bound `drained()` to already-buffered state without polling new groups after cancellation).

Test: a `moq-gst` / `moq-mux` unit test streaming empty groups after cancellation and verifying `drained()` returns immediately.

Public API: none (or small additive read-only method on `moq_mux::container::Consumer`). Wire: none.
