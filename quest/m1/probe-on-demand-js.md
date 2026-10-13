# [M] js/net opens PROBE only while something reads it

## Goal

A js/net connection opens its PROBE stream only while something observes the
receive estimate, and closes it when the last reader goes. A `@moq/watch` that
is playing still gets the estimate (its sync RTT and ABR read it); a connection
nothing reads holds no PROBE stream. The PROBE wire is unchanged.

## Plan

Today `js/net/src/lite/connection.ts` runs `subscriber.runProbe()` for the
whole connection, unconditionally.

Decided (2026-10-12, quest-plan): demand comes from readers, either through
`@moq/signals` observation tracking (subscriber count, mirroring kio's
used/unused) or through an explicit consumer handle mirroring Rust's
`Session::recv_bandwidth()`. Either is acceptable; pick the simpler one once in
the code. With observation, `peek()` does not count, so the watch stats panel
subscribes while it is open. A consumer handle has no `Drop` in JS, so it must
be released by the effect that holds it.

Reconnects (`connection/reload.ts`) carry demand to the new connection.

## Related

- [Rust PROBE on demand](/quest/m1/probe-on-demand-rs.md) - the same outcome for Rust clients
- [Viewer up-switch](/quest/m1/viewer-upswitch.md) - also sends PROBE targets, so it reads the estimate while choosing renditions
