# [L] Transit facts stay read-only

## Goal

Hops, via, source, the epoch and the anonymous mark are facts the origin and
the relays fill in, never something an application sets. `Hop`, `Hops` and
`HopLimit` leave the public API in Rust, JS and the bindings, and a received
announcement exposes its hop chain as plain ids. JS `RouteInfo` stops exposing
`epoch` and `auth`, as Rust's already does not.

Non-goals: the route handle and its options, which
[One Route handle](https://github.com/moq-dev/moq/pull/7806) landed, and the
wire.

## Plan

Split from [One Route handle](https://github.com/moq-dev/moq/pull/7806), which
made `origin::RouteInfo` read-only (getters, no public builders) and moved app
settings to `RouteOptions`, but kept these types public because removing them
reaches every origin construction:

- **Rust.** `origin::Config::new(Hop)` and `Config::hop` take a plain non-zero
  id, `Config::max_hops` a plain count, and `Producer::hop()`/`Consumer::hop()`
  return ids or go crate-private. `RouteInfo::hops()` returns ids rather than
  `&Hops`. `moq_net::fuzz` keeps whatever benches and tests need to build relay
  chains (`fuzz::route_info`, `fuzz::excluding`). About 100 test and bench
  sites build a config with `Hop::new(..)`; moq-relay's cluster id and
  moq-ffi/moq-c already pass plain ids.
- **JS.** Stop exporting `Hop`, `HopSchema` and `isAnonymous` from `@moq/net`'s
  entry, keep `RouteInfo` to instance, cost, hops (as bigints) and anonymity,
  and move `epoch` and `auth` to the internal wire view (`wireOf`) that
  sessions already use.
- **Docs and MIGRATE.** `rs/moq-net/MIGRATE.md`, `js/net/MIGRATE.md` and
  `doc/lib/rs/moq-net.md`.

Run `just check`.

Public API: breaking in moq-net and @moq/net. Wire: none.

## Related

- [Routes are the origin API](/quest/m1/api-audit/route/README.md) - the epic this finishes
