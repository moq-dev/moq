# [M] Cancelling an io_uring handshake releases its connection

## Goal

Dropping a pending QUIC dial or WebTransport handshake closes the abandoned
connection and releases its driver and endpoint bookkeeping while the worker
continues running. No new public API or wire format is needed.

## Plan

The release API audit found that `quic::web::Request::accept` closes its
connection when `handshake` returns an error, but creates its drop guard only
after the handshake completes. Cancellation at an earlier await bypasses that
error arm. Connection drivers retain their own state, so dropping the future's
connection handles does not itself close the connection. The peer can keep it
active beyond the idle timeout. Inspect the equivalent ownership transfer in
`Endpoint::connect` and `connection::establish` too.

Reproduce before changing the code. Establish cleanup ownership before the
first suspension point and transfer it only when the public result is handed
off. Preserve successful sessions and unrelated connections sharing the
endpoint; do not add a timeout or retry to hide abandoned ownership.

Linux CI covers cancellation during QUIC establishment, before peer SETTINGS,
and while awaiting CONNECT. Keep the worker driven, assert the abandoned
connection reaches a terminal state and its bookkeeping drains, and prove a
sibling connection still works. Include the successful handoff case.

## Related

- [Application close delivery](/quest/m1/quic/uring-close.md) - submission and worker teardown after a close is requested
