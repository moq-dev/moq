# [XS] Pin a quiet track's latest group across rejoin and takeover

## Goal

`rs/moq-net/tests/rejoin.rs` proves that a late subscriber to a live, quiet
track receives its latest group after readers left and rejoined and the cache
expiry passed, and after a publisher takeover, on every version, while a
superseded group still expires.

## Plan

`main` already behaves (verified 2026-10-13), but the 0.17 line broke it
([#5413](https://github.com/moq-dev/moq/issues/5413)); the test keeps a
future origin rework from regressing. Decided 2026-10-13 (maintainer): land
the port of the reporter's test plus a takeover variant, on lite-05, 06 and
07 and transport-19 and 22, using `moq_net_sim::test` time. Mention #5413
without a closing keyword; the [release backport](/quest/m0/release-22/quiet-edge.md)
closes it.

## Related

- [Release backport](/quest/m0/release-22/quiet-edge.md) - the 0.17 fix this pins on `main`
