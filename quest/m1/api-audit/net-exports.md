# [S] moq-net exports with a consumer

## Goal

Every public moq-net and `@moq/net` model item added since release has a
consumer in this repository, moq.pro, moq.dev, or an m1 quest. Items without
one go private or away, items that report wrong answers are deleted, and the
JS names for shared concepts match Rust.

## Plan

Decided 2026-10-22 (API audit). Keep the items an m1 quest consumes:
`origin::Preference` and `with_preference` (Rust,
[routes](/quest/m1/cluster-routing/routes.md)), and
`bandwidth::Allocator::set_target` with `transport::Probe`
([native probing](/quest/m1/probe-native.md),
[native rung disable](/quest/m1/simulcast-rung-disable.md)).

Rust (`rs/moq-net/src/model/`), make `pub(crate)` or delete:

- `Route::{auth, with_auth}` (benches only; reopen with
  [in-band auth](/quest/m1/auth/README.md) if it needs them),
  `broadcast::Lister` and `Dynamic::lister` (sessions only, and
  `finish(&self)` does not end it), `stats::Handle::wire`,
  `cache::Pool::gc` (poll-shaped, not named `poll_*`),
  `origin::DEFAULT_UPDATE_HOLD` (external uses are test sleeps).
- `stats::Handle::request` returns a `Session` whose `fail()` does nothing:
  fix or hide.
- `Producer::peer()`/`upstream()` set one private `Link`, and `peer()` after
  `upstream()` silently does nothing: one builder taking the link.
- `auth::Handle::requests()` can fail `Duplicate`; move it to
  `server::Handshake` where it is infallible.
- `moq_net::Role` at the crate root collides with `stats::Role`; rename one.
  `Client::with_cost` takes `impl Into<Cost>`.
- The `async` feature leaves `auth`, `goaway`, and `setup` public but
  unobtainable; gate them with it.

JS (`js/net/src/`):

- Delete `Requesting.unroutable`: reproduced reading `true` while a local
  `dynamic()` claim holds the request, and unused.
- Remove or make internal: `Origin.Preference` and `Producer.withPreference`
  (no JS consumer), `Broadcast.Producer.demand()` and `Broadcast.Demand`,
  `Auth.None`, `Auth.grantsEqual`, `Auth.Handle.requests()` with `Request`,
  `Issued`, `Requests` (P2P is m2), `Epoch.toBytes`/`fromBytes` (wire only),
  `Error.NoStreamCredit` (no Rust twin), and the runtime `Track.liteTimescale`.
- `Track.Demand.priority` is the publisher priority while
  `Track.Request.priority` is the subscriber's; Rust `Demand` has none.
  Remove it from `Demand` and have `Bandwidth.Allocator` read it from `Info`.
- Rename `Track.Location` to `Position`, matching Rust; this cycle already
  changed every method that takes it.
- Stale docs: `{@link update}` and `{@link accept}` in `origin.ts`, `dynamic`
  serving "tracks", undocumented `Timed`, `SessionCode`, `StreamCode`.

Public API: removals and renames in moq-net and `@moq/net`. Wire: none.
