# [S] Measure the io_uring transport's thread check

## Goal

The relay fanout benchmark measures what `moq_uring::transport::Local`'s
per-call thread check costs on the ring now that
[moq-net compiles once](https://github.com/moq-dev/moq/pull/6176), and the
check is removed only if it shows up. `transport::poll::Boxable` stays the
single bound.

## Plan

#5821 serves moq-transport on the ring by wrapping its transport in
`moq_uring::transport::Local` (renamed from `Pinned`), which checks the
thread on every call because the ietf path requires `Boxable`. Decided with
#5821 (maintainer): ship `Local` public now. Kept in m2 (decided 2026-10-17).

Decided 2026-10-22 (maintainer): rescoped from converting moq-transport to
named state machines to measuring the check after
[#6176](https://github.com/moq-dev/moq/pull/6176) boxes the transport. Sweep
the fanout benchmark over sessions and subscribers, with and without the
check, and remove it only if the difference shows.

Public API: none unless the check goes, which removes
`moq_uring::transport::Local`. Wire: none.

## Related

- [Relay io_uring packages](/quest/m2/relay-io-uring-package.md) - not blocked on this; `Local` already gives parity, and only its cost is measured here
