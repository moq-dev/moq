# [M] uring I/O carries its worker and steering identity

## Goal

An endpoint cannot drive a socket through the wrong worker or issue connection
IDs for a different reuseport member. WebTransport setup inherits the worker
that already drives its connection. Keep the worker, UDP, and QUIC layers.

## Plan

`Handle::udp` already associates a socket with a worker, but `Endpoint::new`
takes a separate Handle and `endpoint::Config::shard` is independently
settable. A socket from worker A combined with handle B splits receive I/O
and driver progress across unrelated loops. A wrong shard steers replies to
the wrong socket. `web::Request::accept` repeats the independently supplied
handle next to a connection that already has an owner.

Derive worker identity from the owned socket/connection and carry steering
identity from the completed socket-group member. A plain unsharded socket
remains supported. Remove redundant independently supplied identity rather
than documenting combinations callers must remember. Preserve weak lifetime
links where needed so an I/O handle does not keep a stopped worker alive.

Update the single-connection helpers and in-tree runtime callers with the
same rule. Keep root Config and the existing TxBuf name; those names do not
cause the mismatch. Document exported metrics fields as part of the API pass.

Linux CI covers a stopped owner, two workers on the same thread, a two-member
steered listener, and WebTransport setup. Invalid combinations are either
unconstructible through the public API or refused before tasks or I/O start.
Normal single-worker and multi-worker traffic still works. Preserve the
published moq-tokio worker surface when migrating its internal plumbing.

Public API: breaking construction/configuration changes in moq-uring 0.0.1
and the shared moq-sock member integration. Wire: no format change.

## Required

- [Socket group](/quest/m0/sock-group.md) - supplies completed members with retained socket ownership

## Related

- [Handshake cancellation](/quest/m2/uring-handshake-cancel.md) - cleanup during a suspended handshake without another API
- [PR #3811](https://github.com/moq-dev/moq/pull/3811) - landed the single noq backend before this ownership work
