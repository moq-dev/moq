# [M] Redundant publishers share an epoch, not a hop

## Goal

Nothing names a publisher's identity outside its path. `moq` loses `--hop`,
`MOQ_HOP`, and the hidden `--origin` alias; an optional `--epoch` takes their
place for publishing. Omitted, each run gets a fresh epoch (the origin
default); a redundant pair passes the same value. A plain publisher declares
a random Hop ID per process with no flag, as the bindings and js already do,
so a multi-homed publisher still catches its own loops. `--cluster-id` names
a node and no longer falls back to `--hop`. Relays stop stamping an unnamed
publisher's hop chain, and NO_CAPACITY is gone: every refusal is terminal.

## Plan

- Drop the per-session hop stamp for unnamed publishers (decided 2026-10-03:
  split horizon rests on `via` and own-hop checks, and the stamp never
  matched any relay's id, so it only existed for publisher identity). Delete
  `Hops::stamp` (`rs/moq-net/src/model/origin.rs`), the `stamp` fields in
  `rs/moq-net/src/{lite,ietf}/subscriber.rs`, `stampHops` in
  `js/net/src/hop.ts` and its callers in `js/net/src/{lite,ietf}/subscriber.ts`,
  and their tests, including `rs/moq-net/tests/legacy_reconnect.rs`. Keep the
  leading 0, so an unnamed chain still ranks anonymous.
- Remove NO_CAPACITY entirely (decided 2026-10-03: the maintainer does not
  want a retryable refusal). Every refusal is terminal, and an advertiser
  sheds load by withdrawing or re-pricing its route. That covers
  `StreamCode.NoCapacity` and its uses in `js/net/src/{error,origin}.ts` and
  the reserved-code comments in `rs/moq-net/src/error.rs`.
- Drafts: lite-07 and cluster-02 are in progress, so edit their text and
  changelogs in place. In `drafts/draft-lcurley-moq-lite.md`: the stamping
  rule, NO_CAPACITY in Error Codes and Resolution, "the identity it stamps",
  and "the first entry identifies the endpoint that originated the route". In
  `drafts/draft-lcurley-moq-cluster.md`: the unknown-publisher stamping text,
  the relay behavior that stamps, NO_CAPACITY and its IANA row, and the
  changelog bullets.
- CLI: `rs/moq-cli/src/{args,complete,fetch,ls}.rs`. Relays keep `cluster.id`
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
`--cluster-id` stops reading `--hop`; js/net loses `StreamCode.NoCapacity`.
Wire: relays stop stamping, and lite-07 and cluster-02 lose NO_CAPACITY.

## Required

- [Origin](/quest/m0/broadcast-epoch/origin.md) - the default mint `--epoch` falls back to
