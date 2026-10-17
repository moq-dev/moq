# [L] moq-transport runs as named state machines

## Goal

moq-transport sessions poll named state machines instead of boxed futures,
so the io_uring workers drive them without `transport::poll::Boxable` or
`moq_uring::transport::Local`'s per-call thread check.

## Plan

#5821 serves moq-transport on the ring by wrapping its transport in
`moq_uring::transport::Local` (renamed from `Pinned`), which checks the
thread on every call because the ietf path requires `Boxable`. Decided with
#5821 (maintainer): ship `Local` public now and convert to named machines
later, retiring both. Kept in m2 (decided 2026-10-17).

Measure the per-call cost `Local` adds on the ring before and after, on the
relay fanout benchmark swept over sessions and subscribers.

Public API: `moq_uring::transport::Local` and `Boxable` go away. Wire: none.

## Related

- [Relay io_uring packages](/quest/m2/relay-io-uring-package.md) - not blocked on this; `Local` already gives parity
