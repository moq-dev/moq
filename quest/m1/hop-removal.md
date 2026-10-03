# [M] Redundant publishers share an epoch, not a hop

## Goal

Redundant publishers share an explicit `@<epoch>` instead of a `--hop`, and
the publisher-facing Hop ID is gone: `moq`'s `--hop` / `MOQ_HOP` and the
deprecated `--origin` spelling, and the Hop setup parameter a publisher
sends. An epoch is strictly better, since a Hop ID is per session: one
connection cannot publish several broadcasts with different identities.

## Plan

- The pool semantics already live in
  [Selection](/quest/m1/cluster-routing/selection.md): origins announcing the
  same epoch-qualified concrete path are one source. This quest deletes what
  that makes dead, including `Pin::Publisher(Hop)` in
  `rs/moq-net/src/model/front.rs` and `RouteEntry::qualifies`.
- A redundant pair passes one explicit epoch; the broadcast-publish path keeps
  an epoch a caller supplies ([Broadcast epochs](/quest/m1/broadcast-epoch/README.md)).
  Give `moq` a flag for it if Broadcast epochs has not.
- Relays keep `cluster.id` as their identity; `--hop` fills it today
  (`rs/moq-cli/src/args.rs`), so split that coupling. Relays and P2P peers
  keep the Hop setup parameter: it becomes the node id of
  [Routes and announces](/quest/m1/cluster-routing/routes.md). Only the
  publisher-facing use goes.
- Re-key [Same-hop importers](/quest/m1/hop-aligned-import.md)'s docs and 1+1
  tests to a shared epoch.
- Docs: rewrite "Redundant publishers" in `doc/bin/cli.md` and
  `doc/concept/use-case/contribution.md`, then search the repo for `--hop`,
  `MOQ_HOP`, and `--cluster-id` examples, including demo recipes.

Open: whether anything declares an incumbent dead faster than the QUIC
idle timeout (a failover service that retracts it, or active-active delivery
to the relay). Neither is required to land this.

Public API and wire: removes a CLI flag and the publisher's Hop setup
parameter; lands outside the cluster routing line.

## Required

- [Selection](/quest/m1/cluster-routing/selection.md) - owns the same-epoch pool this relies on
- [Broadcast epochs](/quest/m1/broadcast-epoch/README.md) - a restart is a new path, not a splice
- [Same-hop importers](/quest/m1/hop-aligned-import.md) - the importer fixes this re-keys
