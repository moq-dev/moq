# [M] Redundant publishers share an epoch, not a hop

## Goal

Nothing names a publisher's identity outside its path. `moq` loses `--hop`,
`MOQ_HOP`, and the hidden `--origin` alias; an optional `--epoch` takes their
place for publishing. Omitted, each run gets a fresh epoch (the origin
default); a redundant pair passes the same value. A plain publisher declares
a random Hop ID per process with no flag, as the bindings and js already do,
so a multi-homed publisher still catches its own loops. `--cluster-id` names
a node and no longer falls back to `--hop`. Relays stop stamping an unnamed
publisher's hop chain.

## Plan

- Drop the per-session hop stamp for unnamed publishers (decided 2026-10-03:
  split horizon rests on `via` and own-hop checks, and the stamp never
  matched any relay's id, so it only existed for publisher identity). Delete
  `Hops::stamp` (`rs/moq-net/src/model/origin.rs`), the `stamp` fields in
  `rs/moq-net/src/{lite,ietf}/subscriber.rs`, `stampHops` in
  `js/net/src/hop.ts` and its callers in `js/net/src/{lite,ietf}/subscriber.ts`,
  and their tests, including `rs/moq-net/tests/legacy_reconnect.rs`. Keep the
  leading 0, so an unnamed chain still ranks anonymous.
- NO_CAPACITY's removal (decided 2026-10-03: every refusal is terminal)
  lands with the [wildcard](/quest/m0/wildcard/README.md) line, whose branch
  already deletes it from js/net, moq-net, and both drafts (found in the
  2026-10-05 audit). Don't redo it here; rebase onto that line if it lands
  first.
- Drafts: lite-07 and cluster-02 are in progress, so edit their text and
  changelogs in place. In `drafts/draft-lcurley-moq-lite.md`: the stamping
  rule, "the identity it stamps",
  and "the first entry identifies the endpoint that originated the route". In
  `drafts/draft-lcurley-moq-cluster.md`: the unknown-publisher stamping text,
  the relay behavior that stamps, and the changelog bullets.
- CLI: `rs/moq-cli/src/{args,complete,fetch,announced}.rs`. Relays keep `cluster.id`
  as their node id.
- Docs: `doc/bin/cli.md` ("Redundant publishers" uses `--epoch`), the
  migration row in `doc/setup/upgrade.md`, the stamping paragraph in
  `doc/bin/relay/cluster.md`, and the stamping sentence in
  `doc/concept/moq-lite.md`. Search the repo for `--hop`, `MOQ_HOP`, and
  `--cluster-id` examples, including demo recipes.

Open: whether anything declares an incumbent dead faster than the QUIC
idle timeout (a failover service that retracts it, or active-active delivery
to the relay). Neither is required to land this.

Public API: removes `--hop`, `MOQ_HOP`, and `--origin`, adds `--epoch`, and
`--cluster-id` stops reading `--hop`.
Wire: relays stop stamping.

## Required

- [Origin](/quest/m0/broadcast-epoch/origin.md) - the default mint `--epoch` falls back to
