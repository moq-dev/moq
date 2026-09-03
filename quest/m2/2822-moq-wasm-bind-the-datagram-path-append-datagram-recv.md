# [S] moq-wasm: bind the datagram path (append_datagram / recv_datagram)

## Goal

Implement and verify the behavior tracked in [#2822](https://github.com/moq-dev/moq/issues/2822)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

Split out of #2814, where the Codex reviewer raised it.

\#2814 expands `rs/moq-wasm` to bind moq-net's model: publish, discovery, on-demand serving, fetch, subscription options, track properties, cursors, frame timestamps. Datagrams are the one part of the **track** model it leaves out.

Unbound today:

- `moq_net::track::Producer::append_datagram` / `write_datagram`
- `moq_net::track::Subscriber::recv_datagram`
- the `moq_net::Datagram` value type (`sequence`, `timestamp`, `payload`)

So a browser publisher cannot emit datagrams and a browser subscriber has no way to observe them, even though `@moq/net` exposes `appendDatagram` / `writeDatagram` / `datagrams` and the browser transport carries them.

This is reachable, not theoretical: datagrams flow on moq-lite-05 over a transport that supports them, and `rs/moq-wasm/src/transport.rs` already implements the datagram side of `web_transport_trait::Session`. They are dropped on IETF moq-transport, moq-lite before 05, and stream-only transports like WebSocket, so any binding should make that conditional obvious rather than looking like a silent no-op.

Deliberately deferred out of #2814 rather than rushed in: everything else in that PR was verified in a browser against a relay, and shipping datagram bindings without the same treatment would be the one untested corner of a new public surface. A datagram round trip needs its own harness (publish a datagram, confirm it arrives, and confirm the documented drop on a version that can't carry it), which is more than a review pass should bolt on.

`rs/moq-wasm/README.md` lists this under "Not covered" so the surface doesn't overclaim in the meantime.

Related: #2816 (no browser test harness at all), which is what would make verifying this cheap.

## Closes

- [#2822](https://github.com/moq-dev/moq/issues/2822) - close this issue when the quest finishes
