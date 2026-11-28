# [M] Demote a stale JS session's routes

## Goal

When an `@moq/net` session's `keepAlive` reports its peer `stale`, that session's routes rank as draining, and they are restored once the peer is heard again. This mirrors Rust's stale-route demotion ([#9838](https://github.com/moq-dev/moq/pull/9838)).

- `keepAlive` (#9698) runs over QMux WebSocket and Node QUIC. Browser WebTransport cannot probe, so it is out of scope.
- The session stays open, the wire is unchanged, and a GOAWAY still lasts after recovery.

## Plan

- Both connections already hold the session's `liveness` getter (`lite/connection.ts`, `ietf/connection.ts`) but never pass it to their subscribers.
- Combine GOAWAY (lasts) and staleness (clears) into one draining signal, the way Rust's `goaway::Draining` does.
- Feed that signal into the Lite subscriber (lite-05/06 and the lite-07 routed path) and the IETF subscriber.
- Demotion must be reversible:
  - Existing routes and routes announced while stale rank as draining.
  - On recovery, clear only the local stale mark, keeping any GOAWAY or upstream drain.
- Make the tracked drain state the one value both new routes and the run read, so a flip between passes cannot strand a route draining. Rust's fixes in #9838 for `lite/subscriber.rs` `Announced` and `ietf/subscriber.rs` `drain_route` show the two races to avoid.
- Test on lite-06, lite-07 and moq-transport: stale demotes, heard again restores, and a GOAWAY outlasts recovery.

## Related

- [#9838](https://github.com/moq-dev/moq/pull/9838) - the Rust side, and the decision to defer this
