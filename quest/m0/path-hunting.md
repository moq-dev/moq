# [M] A withdrawn path stops hunting

## Goal

When a broadcast ends, each relay in a lite-06 mesh retracts it about once,
instead of re-announcing stale alternatives. Today one withdrawal on live's
34-relay graph costs hundreds to over a thousand extra announce starts, and
on Kick dev an ended name drew 575 to 57,805 mesh announce starts (median
about 13,000) while live names drew one flood (moq-dev/moq.pro#2116). New
and removed routes still propagate at once, and no wire changes.

## Plan

Promoted from the cluster routing line to m0 on 2026-09-30: it is today's
partial-mesh hunting that #4399 left open, and it is hurting production.

Decided (2026-09-30), lite-06, no wire change:

- A route *update*, where a relay's advertised route for a path switches to a
  different route, waits a short hold-down before it is re-advertised. A new
  path (nothing advertised yet) and a removal (nothing left) go out at once.
- So when the best route is withdrawn and an alternative remains, the relay
  retracts at once and re-announces the alternative only if it survives the
  hold-down. Stale alternatives are withdrawn inside that window and never
  spread. A genuine failover, where a link dies but the origin still
  publishes, is delayed by the hold-down; pick the value from the repro, and
  report the failover cost it adds.
- Check the interaction with #4399's withdrawal hiding and with
  subscriptions already pinned to a route (fronts keep serving from a live
  source; only advertisements are held).

Test first: land moq.pro's in-process repro upstream (real lite-06 sessions
over the mock transport on live's 34-relay graph, mocked time), asserting
about one retraction per relay for a single withdrawal from a non-hub
publisher, plus a failover test where a link dies and the path comes back
through another route after the hold-down. It must fail on `main` today.

Then moq.pro moves its pin and confirms about one retraction wave per ended
name on Kick dev after a deploy the user approves (moq.pro's own quest).

If no hold-down closes it, the fallback is a per-origin seqno on cluster
links in the wip lite version; report before building it.

Public API: none expected. Wire: none.

## Related

- [Cluster routing](/quest/m1/cluster-routing/README.md) - tiers shrink the core graph this hunts on
- [Cross-relay delivery under bursts](/quest/m1/cross-relay-bursts.md) - closed broadcasts announced for minutes, the same symptom
