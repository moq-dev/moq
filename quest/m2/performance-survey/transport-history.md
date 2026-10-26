# [M] Transport preference has a justified lifetime

## Goal

Connection racing retains useful fallback knowledge without accumulating
unneeded URL history for the whole page lifetime.

## Plan

[Connection racing](/js/net/src/connection/connect.ts) stores each WebSocket
winning full URL in module-level `websocketWon`, with no deletion. A later
WebTransport win does not clear it. Unique paths or rotating query strings
remain after pooled connections expire. Comments call this the last race,
while current behavior remembers whether WebSocket ever won.

Sweep unique URLs × reconnects per URL, varying URL length and synthetic
query rotation. Close sessions, advance beyond pool linger and force GC in
the browser harness. Measure retained strings/heap, connection latency and
transport selection. Include WebSocket then WebTransport wins, alternate
fallback URLs and disabled fallback. Never put real credentials in reports.

Establish the useful ownership lifetime before changing it. Do not silently
collapse endpoint-specific behavior into origin-only keys or pick an
arbitrary TTL; bring a necessary racing-policy decision to the maintainer.

Follow the [survey benchmark contract](/quest/m2/performance-survey/README.md):
baseline first, paired evidence, equivalent work and nightly coverage. A
measured no-win is valid. No API or wire change is expected; report actual impact.
