# [S] An advertisement stuck on a zero-credit peer gives up

## Goal

A Rust moq-transport publisher whose PUBLISH_NAMESPACE or namespace update
cannot leave because the peer grants no send credit gives up on it within a
bounded time, as it already does on a silent answer: it resets the request
and frees its state. Withdrawal still frees it at once, and a peer that
grants credit late but in time is unaffected.

## Plan

Decided 2026-10-31 (user, /quest-plan of the follow-ups from the 2026-10-31
/quest-complete run), from
[moq-dev/moq#7414](https://github.com/moq-dev/moq/pull/7414):

- Bound a stuck advertisement under a zero-credit peer:
  - ✅ Quest in m2. Withdrawal already frees it after [moq-dev/moq#7414](https://github.com/moq-dev/moq/pull/7414), so the
    remaining cost is one buffered request per namespace, not a leak.
  - Drop it.

[moq-dev/moq#7414](https://github.com/moq-dev/moq/pull/7414) (open, owns [the withdraw quest](/quest/m1/ietf-withdraw-credit.md))
buffers each message and starts `ADVERTISE_TIMEOUT`
(`rs/moq-net/src/ietf/publisher.rs:513`, 5 s) only once the bytes reach the
transport; its `NamespacePending::Writing` state has no deadline, and the PR
notes "send backpressure alone does not start answer timeouts or retries".
Build on it once it lands:

- Give `Writing` a deadline too, reusing `ADVERTISE_TIMEOUT` from when the
  message was buffered rather than adding a second constant, and on expiry
  take the same path as an unanswered request (reset both directions, end
  the request).
- Test on drafts 18 and 22 with the zero-credit test peer [moq-dev/moq#7414](https://github.com/moq-dev/moq/pull/7414) adds: a
  never-granting peer times out the initial advertisement and an update; a
  peer granting credit before the deadline is answered normally.

Public API: none. Wire: none.

## Related

- [A withdrawn moq-transport advertisement stops writing](/quest/m1/ietf-withdraw-credit.md) - [moq-dev/moq#7414](https://github.com/moq-dev/moq/pull/7414) lands this first and adds the buffering this bounds
- [One request lease](/quest/m2/ietf-request-lease.md) - other request lifetimes on the same streams
