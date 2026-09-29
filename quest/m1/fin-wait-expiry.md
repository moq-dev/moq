# [S] A group awaiting its FIN ack still expires

## Goal

A group whose FIN is written but not yet acknowledged still expires with
`Old` and still follows priority updates, on lite and IETF. Today the
`Closed` arm only awaits `writer.poll_close`, so a stale group keeps its
queued bytes and its old send order, which can stall newer groups behind it.

## Plan

The issue ships four failing tests, a `with_fin_gate` test hook, and a fix:

- lite: poll priority above the state match, not only in `Serve`.
- lite and IETF: in `Closed`, poll `poll_expired_while_pending` and abort with
  `Old`.
- moq-tokio: `set_priority` in `Closing` fires the existing interrupt so the
  new order applies, instead of only storing it.

Each priority update rebuilds the `closed()` watch; check the publisher
benchmarks for a regression. The flaky
`subscription_end_integrity::a_subscription_cut_by_the_publisher_disconnecting_does_not_end_clean`
the reporter hit is tracked in [More tests under load](/quest/m1/test-flakes-2.md).

## Closes

- [#4332](https://github.com/moq-dev/moq/issues/4332) - close this issue when the quest finishes

## Related

- [Starvation](/quest/m1/qos/starvation.md) - its frontier relies on FIN acks, which can now end in `Old`
