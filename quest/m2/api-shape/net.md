# [M] moq-net's small surface fixes

## Goal

Each item below lands in moq-net (and moq-auth for the mesh constants) with
its JS and binding mirrors:

- Remove `Client::with_origin` (`rs/moq-net/src/client.rs`). It is sugar
  for `with_publisher(&o).with_subscriber(o)`, release's
  `Server::with_origin` is already gone, and its six callers (moq-boy,
  moq-stats, the relay's cluster and stats, a moq-ffi test, and moq-tokio's
  forwarder) switch to the two calls.
- Make `Version::{code, from_code}` `pub(crate)`. The relay compares
  `version.code() >= 0xff0dad05` (`rs/moq-relay/src/cluster.rs`) and moq-cli
  scans `0xff0dad00..=0xff0dadff`; each asks a named question of `Version`
  instead (add the predicate the relay needs, and let moq-cli list
  `Version`'s supported set).
- Make `group::Producer::demand` and `group::Demand` `pub(crate)`. Nothing
  outside moq-net calls them, and flights stop through
  `Fill::poll_unwanted`. Update `rs/moq-net/MIGRATE.md`'s mention.
- Replace `compress: bool` on track info with a `Compression` enum that
  matches the wire's values, in Rust, `@moq/net`, moq-ffi, and moq-c.
- Rename `server::{Activity, Active}` to `server::{Admission, Admitted}`.
- Rename `RefusalKind::Lan` and `Refusals.lan` to `Membership` and
  `membership` ("a membership proof was missing or invalid"), so moq-net's
  counters name no relay flag. No serde alias: the field is new since
  release. moq-stats and stats dashboards follow.
- Add `reconnect::Attempt::client(&self, &Client) -> Client`, which sets the
  URL, places the token where the client's own versions put it, and attaches
  the token handle. It replaces `Attempt::token()`, `Reconnect::with_tokens`'s
  `versions` argument, and the caller's separate `with_auth` call.
- Make `moq_auth::mesh::{PROOF_CONTEXT, CHALLENGE_LEN, proof_message}`
  private.
- Rename the datagram `Producer::poll_priority(waiter, last)` to
  `poll_priority_changed(&mut self, waiter)`, keeping its own last value like
  `poll_wanted_changed`.
- `Producer::route` and `Request::route` refuse an instance that
  `is_minted()`, so a caller cannot claim a random instance as stable.
- After [moq-dev/moq#8063](https://github.com/moq-dev/moq/pull/8063) removes
  `RouteEvent::Takeover`, un-hide `origin::Consumer::routes`, `Routes`, and
  `RouteEvent`, documented as the relay-level stream that moq-relay and
  downstream relays already consume, and fix `request`'s doc to say what #8063 made it
  do. `RouteEvent` becomes `pub struct RouteEvent { pub kind: RouteKind, pub
  announce: Announce }` with `pub enum RouteKind { Start, Update, Restart, End }`,
  mirroring JS's `{ kind } & Routed`, so callers need no or-pattern and no
  accessors.
- `stats::Registry::report` takes an explicit `now`, and
  `stats::Handle::{refuse, session}` take `#[non_exhaustive]` argument
  structs with a `new(label, ..)` constructor (named by role, not clashing
  with `server::Admission`), so [lag-histogram](/quest/m3/qos/lag-histogram.md)
  and [session-outcome-records](/quest/m3/session-outcome-records.md) add
  fields instead of breaking moq-net again.
- `group::MAX_CACHE_BYTES` becomes `pub(crate)`: byte-holes turns it from a
  size cap into a tail budget, and moq-json's `max_group_bytes` keeps its own
  32 MiB default. Each outside use takes a limit of its own: moq-json's
  encoder defaults and clamp, moq-mux's jitter, container-consumer, and
  fMP4 bounds, the moq-stats `producer_tick` benchmark, and the oversize
  tests in moq-json, moq-mux, and moq-gst.
- Remove `origin::Consumer::with_proxy` (from
  [moq-dev/moq#8023](https://github.com/moq-dev/moq/pull/8023)) in favor of
  `origin::Producer::with_proxy`, which already reaches every consumer taken
  from that producer. Only tests call the consumer override: the moq-net
  integration harness (`rs/moq-net/tests/integration/support/harness.rs`, two
  sites), `mount.rs`, and moq-mux's `container/test_util.rs` (two sites);
  each sets proxy on the producer it consumes from instead.
- `auth::Grant` gets `#[non_exhaustive]`, since
  [rate-grant](/quest/m5/rate-grant.md) adds rates to it; callers outside
  moq-net build it from `Grant::default()`.

## Plan

Found by the 2026-11-07 API shape audit (net model and net session); see
the [epic](/quest/m2/api-shape/README.md). Each item is XS except
`Attempt::client`, `Compression`, and the `with_proxy` removal (S each). `RefusalKind` is a stats-wire
field, so say in the PR which deployed dashboards read it. If #8063 has not
merged when this starts, leave the un-hide for a follow-up and say so.

Added 2026-11-07 (maintainer, /quest-plan from the 2026-11-07 API sweep of open PRs and remaining quests): the `RouteEvent`
struct, the stats arguments, the cache cap, and `Grant`. The `RouteEvent`
reshape replaces [moq-dev/moq#7862](https://github.com/moq-dev/moq/pull/7862)'s
accessors. #7862 closed without taking it, so the reshape lands here.

Decided 2026-11-07 (maintainer, /quest-complete, from
[moq-dev/moq#8088](https://github.com/moq-dev/moq/pull/8088)'s follow-ups):
✅ remove `origin::Consumer::with_proxy` in favor of the producer's.
