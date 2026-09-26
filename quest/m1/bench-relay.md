# [L] Relay session benchmark through moq-relay

## Goal

The sans-IO session bench also runs through `moq-relay`'s own connection
handling, including auth, the cluster origin, and stats, over the in-memory
transport. Relay-layer costs then show up in benchmarks, not only the
`moq-net` model.

## Plan

`moq_relay::Connection` takes a `moq_tokio::server::Request`, which wraps a
concrete transport, so there is no seam for an in-memory session today. Find
the smallest change that lets the handler run over a generic `moq-net`
transport session without widening the public API. If the seam costs more
than the bench is worth, say so and stop.

Reuse the scenario, the publisher and subscriber sweeps, and the delivery
accounting from `rs/moq-net/benches/session.rs` so the two results line up, and
the difference is the relay layer.
