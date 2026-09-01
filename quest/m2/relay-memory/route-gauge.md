# [XS] Routes per broadcast gauge

## Goal

An operator can see how many routes a relay holds for a path. moq.pro's
(downstream) announce gauge wants it, and nothing exposes it.

## Plan

This rode the standby-routes quest as a passenger, because that quest had the
route table open anyway. [moq#3225](https://github.com/moq-dev/moq/pull/3225)
completed standby routes by making a non-selected route a table entry, so the
quest was closed and the accessor needs its own change.

Expose the count of routes covering a path from `origin`, next to whatever
`best_server` already walks, and surface it wherever `moq-relay` reports node
state. Prefer a count over exposing the entries themselves: the caller is a
gauge, and a routes iterator is a much larger surface to keep stable.
