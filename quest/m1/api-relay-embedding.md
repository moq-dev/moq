# [L] Preserve relay runtime ownership for embedders

## Goal

An application adds routes and uses relay handles without copying startup,
worker serving, shutdown, and joining logic or silently dropping new listeners.

## Plan

At dev `e2350b39a`, `rs/moq-relay/src/relay.rs:9-37` recommends destructuring
the non-exhaustive Relay with `..`, claiming startup changes arrive safely
through load. Public workers and uring fields own bound sockets (`:73,81`);
discarding them can remove QUIC while the embedding code still compiles.
The warning names workers but omits uring. Serving correctly requires the
lifecycle logic in `Relay::run` (`:306` onward).

Actual moq.pro edge code uses that embedding pattern and its own serving loop
(`rs/edge/src/main.rs:88,317` in `/home/kixelated/work/moq.pro`). Its dependency
is older; the finding is the silent migration hazard under worker/uring
configuration, not a claim that its current deployed configuration is broken.

Decided: an owning relay runner with custom routes and borrowed/cloned
application handles. Retain startup, listener selection, worker ownership,
error propagation, and shutdown joins inside the owner. This is distinct from
fixing the worker implementation itself or designing new drain semantics.

Add an external-crate example/test with a custom route and live QUIC request
under shared Tokio, worker Tokio, and Linux io_uring. Prove stopping the owner
releases listeners and joins workers. Wire every supported configuration into
normal or nightly CI. Update relay docs in the same change.

Public API: breaking embedding ownership. Wire: no format change. Run relevant
relay checks/tests and `just test smoke-full` if gateway paths change.

## Related

- [Worker lifetime](/quest/m1/2964-quic-workers-dropping-one-split-server-resizes-the.md) - correctness inside a split worker group
- [Relay drain API](/quest/m2/drain/relay-drain-api.md) - policy for new arrivals during drain
