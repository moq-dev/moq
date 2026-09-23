# [M] An io_uring dial resolves once its last handshake flight is sent

## Goal

`moq_uring::quic::client::connect` hands back a connection only after the
handshake's final flight has been written to the socket, so a worker that
stops right after the dial resolves cannot strand a peer that is still waiting
for it. A worker that stops before that point fails the dial loudly. No new
public API or wire format.

## Plan

Today the dial resolves when the handshake completes locally, while the
client's last flight is still queued behind pacing on the worker (#3865). If
the worker stops in that window nothing is sent, the server times the
connection out, and the client holds a connection that looks established. The
behaviour is documented but easy to hit from a short-lived task.

Reproduce it first on Linux CI: dial, stop the worker immediately, and show
the server never accepts. Then keep the dial future pending until the driver
reports the handshake flight flushed, and return `quic::Error` when the worker
stops before then. The flush belongs in moq-uring's connect and establish
path; moq-tokio is unaffected. Do not add a timeout or a retry, and do not
drain sends from worker shutdown.

Linux CI covers the stopped-worker dial (now refused or fully sent, never
stranded), a normal dial still resolving promptly, and a paced flight on a
slow link. Update the moq-uring docs where the window was described.

Public API: none. Wire: none.

## Related

- [io_uring handshake cancellation](/quest/m1/uring-handshake-cancel.md) - the same establish path, for a dropped dial
- [uring identity](https://github.com/moq-dev/moq/pull/3865) - where the window was found
