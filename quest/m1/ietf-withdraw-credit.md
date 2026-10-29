# [S] A withdrawn moq-transport advertisement stops writing

## Goal

Withdrawing a namespace a Rust publisher advertised over moq-transport
releases its state at once, even when the peer's send window holds back the
PUBLISH_NAMESPACE or a later namespace update.

## Plan

Found by [moq-dev/moq#7016](https://github.com/moq-dev/moq/pull/7016),
which fixed the subscriber side. In `rs/moq-net/src/ietf/publisher.rs`,
`advertise_namespace` awaits its PUBLISH_NAMESPACE, and each namespace
update, before it watches for withdrawal, so a peer that withholds
flow-control credit pins a withdrawn advertisement.

Decided 2026-10-28 (maintainer): its own quest rather than part of
[one request lease](/quest/m2/ietf-request-lease.md), since a peer making us
hold state is a hostile-peer bound to fix now; it waits for
[Rust FETCH parity](/quest/m1/ietf-fetch-parity.md) only because that PR edits
the same file. Use #7016's pattern: buffer each message and flush it inside
the wait that already watches for withdrawal, and reset the stream rather
than FIN it when withdrawn before the message fully left.

Test with a peer that never grants send credit, on drafts 18 and 22.

Public API: none. Wire: none.

## Required

- [Rust FETCH parity](/quest/m1/ietf-fetch-parity.md) - edits `publisher.rs`, so this follows it
