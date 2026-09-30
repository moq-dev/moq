# [S] Remove cluster gossip discovery

## Goal

A relay dials only peers an operator configured: `cluster.connect`, the
connect API, or LAN mDNS. Gossip discovery (`cluster.mesh`), which advertises
under `.internal/origins/` and dials whatever URL appears there with
`cluster.token`, is removed. It is the only path that dials a URL learned
from an announcement. The design favors a configured full mesh, and moq.pro drives peers
through `--cluster-connect-api` and never enables gossip.

## Plan

- Delete gossip advertising and discovery (`rs/moq-relay/src/cluster.rs`,
  `nodes.rs` `MESH_PREFIX`) and whatever becomes dead behind them. Keep
  `cluster.node` only if something else still reads it.
- `nodes.rs` also backs the internal `/nodes` endpoint (`internal.rs`
  `serve_nodes`), which merges gossiped advertisements with direct sessions.
  It keeps direct sessions only; document the changed response.
- Retarget `rs/moq-relay/tests/hidden_cluster.rs` to another `.`-prefixed
  hidden broadcast, and fix the `.internal/origins` comment in
  `connection.rs`.
- Lands on `main` as a security fix, not `dev`. `--cluster-mesh`,
  `MOQ_CLUSTER_MESH` and `mesh = true` fail at startup with a message
  pointing at `cluster.connect`, so nobody silently loses their mesh.
  Decided 2026-09-29.
- Docs: rewrite the Discovery section of `doc/bin/relay/cluster.md`, drop the
  `.internal/` warning, and add an upgrade note in `doc/setup/upgrade.md`.
  Search the repo for `cluster-mesh` and `mesh = true` in examples and demo
  recipes.

Public API: removes a relay config field and flag. Wire: relays stop
announcing `.internal/origins`.

## Related

- [Cluster routing](/quest/m1/cluster-routing.md) - takes its topology from configured links only
