# [XS] Cluster peers in moq-relay lose the egress field

## Goal

A cluster peer, in `cluster.connect` or an `api.url` response, has no
`egress` field, and `cluster::Peer` has no `egress` or `with_egress`.

## Plan

Decided 2026-11-21 (maintainer, /quest-plan of the main → release routing
audit): ✅ remove it before 0.22 publishes main's version (rejected: keeping
it as a reserved field). On `release` it declared this relay's own price to
the peer. Since [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023)
its only behavior is an error unless it equals `cost`, "until asymmetric
routing lands" (`rs/moq-relay/src/cluster.rs:50-62`, `306-315`), and nothing
plans asymmetric routing. Remove it from the API schema
(`rs/moq-relay/src/cluster/api.rs:14`), docs, and examples too.

Public API: `cluster::Peer::egress` and `with_egress` go. Wire: none.
Migration note: delete `egress` from peer objects; a config that still sets
it is refused as an unknown field.
