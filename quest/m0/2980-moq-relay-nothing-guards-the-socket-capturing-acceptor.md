# [M] moq-relay: nothing guards the socket-capturing acceptor being installed on a listener

## Goal

Implement and verify the behavior tracked in [#2980](https://github.com/moq-dev/moq/issues/2980)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Follow-up from #2963, raised independently by two review passes and accepted there rather than blocking the merge.

#### The gap

`SocketAcceptor` (plain HTTP) and `MtlsAcceptor` (HTTPS) each capture the connection's descriptor so qmux can report an RTT at SETUP time. `web.rs` tests cover the capture itself and its propagation onto the request, but nothing covers the *installation*: deleting `.acceptor(SocketAcceptor)` from `Web::serve` leaves the whole suite green.

That is not hypothetical. #2963 originally shipped exactly that bug  -  only the HTTPS listener installed an acceptor, so every `ws://` session silently advertised no Probe capability and stayed on the fallback jitter buffer. It looked correct in any TLS deployment and did nothing for local development and the demo.

#### Why it was not fixed there

Two obvious approaches were considered and rejected:

- **Install the acceptor inside `listener::server()`** so no call site can forget. `internal.rs` shares that helper and serves only an ops router, so this would duplicate a descriptor on every health-check connection for nothing.
- **Assert it end to end** in `tests/smoke.rs`, which does have a working `ws://` relay harness. `moq_net::ConnectionStats` exposes `estimated_recv_rate` from PROBE but not the PROBE's RTT, and the relay's send-rate estimate is `None` on macOS, so there is no cheap observable that proves the capture reached qmux.

#### What would close it

Surfacing the peer-reported PROBE RTT on `ConnectionStats` would make the smoke assertion straightforward, and is plausibly useful to consumers in its own right  -  an application on the adaptive-jitter path currently cannot see the RTT its buffer is derived from. That is a public API addition and deserves its own design pass rather than being bolted on to satisfy a test.

## Closes

- [#2980](https://github.com/moq-dev/moq/issues/2980) - close this issue when the quest finishes
