# [S] Whether an origin forwards is set once, in origin::Config

## Goal

Whether an origin forwards (`proxy`) is fixed when the origin is created, in
`origin::Config`, and `with_proxy` leaves the handle API. A session that
must not forward uses `Link::Upstream`. No order of handle creation changes
how the relay marks or stamps claims.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the main → release routing
audit): ✅ move it into `origin::Config` (rejected: keeping `with_proxy` and
failing loudly when two handles disagree, which leaves a per-handle knob
that can only ever hold one value). The cut waits on it.

Reproduced 2026-11-21 with a scratch test:

- The route node freezes `proxy` from whichever handle calls `route_node()`
  first (`rs/moq-net/src/model/origin.rs:7856`, `:7864`), while per-session
  roles still follow each handle (`node_id` at origin.rs:7798,
  `lite/subscriber/routed.rs:327-340`, `route_stream.rs:233`).
- If an embedded application's handle with `with_proxy(false)` creates the
  node first, claims learned from lite-06 come out marked, and their chain
  is not stamped (`[10]` instead of `[10, 1]`). Marked claims never go out
  over upstream links, and the unstamped chain stops catching loops
  through this relay across scoped grants.
- In the reverse order, the application's proxy-off port is likely told
  unmarked claims (read from `route/relay.rs:354-356`, not tested).
- moq-relay hardcodes `with_proxy(true)` (`rs/moq-relay/src/cluster.rs:1518`);
  other callers are tests (`rs/moq-net-tests`, `rs/moq-mux` test utilities).
  JS has no proxy switch.

Caused by: likely [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023),
which first created the route node from a handle; confirm by diff.

Public API: `with_proxy` moves to `origin::Config`. Wire: none. Migration
note: set `proxy` on the origin's config, and confine a session that must not
forward with `Link::Upstream`.
