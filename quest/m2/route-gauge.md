# [XS] Routes per broadcast gauge

## Goal

An operator can see how many routes a relay holds for a path. moq.pro's
(downstream) announce gauge wants it, and nothing exposes it.

## Plan

Expose the count of routes covering a path from `origin`, next to whatever
`best_server` already walks, and surface it wherever `moq-relay` reports node
state. Prefer a count over exposing the entries themselves: the caller is a
gauge, and a routes iterator is a much larger surface to keep stable.

## Related

- [Relay memory](/quest/m2/relay-memory.md) - the remeasurement this gauge helps read
