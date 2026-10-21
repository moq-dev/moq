# [S] The draft-16 cancel-ahead test settles deterministically

## Goal

`ietf::publisher::tests::a_draft_16_cancel_ahead_of_the_answer_ends_only_its_request`
(`rs/moq-net/src/ietf/publisher.rs`) passes every run. It failed 1 of 30
stressed runs while [#6051](https://github.com/moq-dev/moq/pull/6051) was
landing, although it runs on `moq_clock::test`, so the cause is an ordering
race rather than wall time.

## Plan

Seen during the 2026-10-20 merge batch. Ranked first among the new flakes:
with the clock mocked, a nondeterministic result points at task or message
ordering in `run_publish_namespaces`, which may be a product bug.

- Reproduce in a loop and capture the failing assertion before changing
  anything.
- The test queues the answer and PUBLISH_NAMESPACE_CANCEL as one read, then
  swaps the route's auth and waits for the namespace to be offered again. Check
  whether the cancel can be read before the update is written, or the re-offer
  can race the cancel's withdrawal, and what a real draft-16 peer would see in
  that order.
- If the order is legal on the wire, the publisher must handle it the same
  way every time; fix the code. If only the harness reorders (`settle()` and
  `poll_until` interleaving), make the harness deterministic.

Public API: none. Wire: none.
