# [S] moq-net exports only what a consumer calls

## Goal

Every `pub` item in moq-net has a consumer outside its own tests, sits under
the module that names its role, and takes `&self` when the handle it is on
is `Clone`. What is left after the sweep is the surface the release keeps.

## Plan

Delete, verified against this repository and moq.pro:

- `Hops::replace_first`, `origin::Dynamic::{hop, root}`, `DRAIN_COST` and
  `MAX_COST` (fold into `Cost::{DRAIN, MAX}`), `Cost: From<(u64, u64)>`,
  `Route::with_hop`, `broadcast::Producer::remove_track`,
  `track::Producer::start_sequence`, `Subscriber::with_groups`,
  `Ordered::with_groups`, `group::Consumer::with_frames` (the `set_*` forms
  are what every caller uses), `cache::Pool::same_pool`,
  `Timestamp::new_const` (private; it only feeds `ZERO`).
- `Error::to_code`: nothing wire-facing uses it and it is not injective
  (`FrameOpen` and `MalformedTrack` share 22, `Closed` and `SessionClosed`
  share 25). The registries are the wire.

Rename under their modules: `track::SubscriberControl` to `track::Control`,
`track::GroupRequest` to `group::Request`, `moq_net::ConnectionStats` to
`session::Stats` with `estimated_send_rate`/`estimated_recv_rate` typed as
`Option<bandwidth::Rate>`, and the paused handshake `moq_net::Request<S, R>`
to `server::Handshake` so it stops colliding with `origin::Request` and
`track::Request`.

Shape fixes: `create_track`, `reserve_track`, `unique_track`, `finish`,
`create_group`, and `append_group` take `&self` (each only locks a
`kio::Shared` and the handle is `Clone`; moq-stats threads `&mut` through
`adopt` for no reason); `track::Consumer::info() -> Pending<Querying>`
becomes `query()` so `info()` means the same as on `Subscriber`;
`track::Demand` gains the `is_used`/`poll_used`/`poll_unused` that
`broadcast::Demand` has and `track::Producer::poll_unused` returns what
`unused()` returns; `bandwidth::Producer::closed()` returns the cause like
every other `closed()`.

Docs: `lib.rs` says `Client::with_stats` takes a `stats::Handle` (it takes
`stats::Session`); `doc/lib/rs/moq-net.md` still describes an ordering
preference on tracks. Document the flat-versus-nested `Error` rule (local
versus received) on the enum; folding the two registries is a later
decision.

Public API: breaking on moq-net, so on dev. Wire: none. Run the rustdoc
lint after the renames.

## Related

- [Announce event](/quest/m1/api-net-announce.md) - the one moq-net shape decided separately
