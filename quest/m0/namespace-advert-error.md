# [S] One namespace's stream error ends only its own advertisement

## Goal

On moq-transport, a Rust publisher whose PUBLISH_NAMESPACE request stream
fails while it waits for the first answer withdraws just that namespace, as
JS does, and keeps advertising and withdrawing every other namespace.
Today `advertise_namespace` propagates the error with `?` out of
`run_publish_namespaces` (`rs/moq-net/src/ietf/publisher.rs`), and the
session races that loop, so one reset PUBLISH_NAMESPACE stream closes the
whole session and aborts every track on it. When the error is a
`ProtocolViolation`, moq-tokio treats it as terminal and stops reconnecting.

## Plan

Found by [#5998](https://github.com/moq-dev/moq/pull/5998), which handles
GOAWAY throughout IETF request lifetimes; the bug predates it. Decided
2026-10-20 (maintainer): m0, since a single hostile or buggy peer stream ends
the session on main. JS already contains the failure per namespace
(`#advertise` in `js/net/src/ietf/publisher.ts`); match it.

- Separate a request's own failure (its stream reset or ended, a refusal)
  from a session failure (the control stream, a protocol violation that
  closes the session at the source). The first ends that namespace's request
  and leaves the loop running; the second still ends the loop.
- Re-offer a namespace whose request failed on the same backoff #5998 added
  for a draft-18+ stream that ends while idle (100 ms, doubling to 5 s), so a
  peer that resets every advertisement gets backed-off re-offers, not a
  request loop.
- Regression test: two namespaces advertised, the peer resets the first's
  request stream before answering; the second is still answered and
  withdrawn normally, and the first is offered again after the wait.

Public API: none. Wire: none.

## Related

- [Drain cleanup](/quest/m1/drain-cleanup.md) - what a drain does to sessions at its deadline
