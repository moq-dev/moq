# [S] An origin is scoped in one call and defaults to a real hop

## Goal

`origin::Producer` and `origin::Consumer` read like the rest of moq-net:
a scoped handle comes from one fallible call that says why it failed, a
fresh origin has a hop of its own, and the handle is not a pointer to its
hop.

## Plan

- `origin::Config::default()` mints `Hop::random()`. 195 call sites here
  and 48 in moq.pro spell `Config::new(Hop::random())` or
  `spawn(Hop::random())` today, and none of them wants loop detection off.
  `moq_tokio::origin::spawn()` then takes no argument for the common case.
  Decide the `TEMPORARY` 53-bit cap in `origin.rs` at the same time; JS
  already reads a full u62.
- `broadcast::Info` stops embedding an `origin::Config`. Nothing reads a
  broadcast's `origin.id`; only the cache pool is used, so `Info { pool,
  path }` with `create_broadcast` handing the origin's pool down. Otherwise
  a random-hop `Default` makes every standalone broadcast (every
  `Info::new()` in moq-json) mint an identity nobody reads and a pool of its
  own.
- `with_root(prefix)?.scope(&patterns)` becomes
  `scope(root, &Patterns) -> Result<Self, Error>` returning `Unauthorized`
  for an empty intersection or a root nothing lies under, on both Producer
  and Consumer. PR #3746 makes `scope` intersect any union and keeps both
  calls returning `Option`; the two-step chain is at every auth site
  (`rs/moq-relay/src/cluster.rs`, `rs/moq-cli/src/auth.rs`,
  `rs/moq-ffi/src/origin.rs`) and `None` carries no reason.
- `routed_broadcast` and `request_broadcast` agree on the out-of-scope
  error: `Unauthorized`, matching `create_broadcast`. `request_broadcast`
  says `Unroutable` today and an unparseable path surfaces as `Closed`.
- Drop `impl Deref<Target = Hop>` on both handles; add `hop()` and rename
  `Config.id` to `Config.hop`, the spelling `Request::peer_hop` and
  `Dynamic::hop` already use.
- `origin::Pending` becomes `origin::Requesting` beside
  `track::{Subscribing, Querying, Fetching}`; moq-stats writes
  `kio::Pending<origin::Pending>` today.
- `origin::Producer::publish(path, route) -> Result<broadcast::Producer>`
  folds `create_broadcast` plus `announce`; moq.pro wrote that helper in
  three crates and one comment flags the window between the two calls.
  The connect side stops reading `with_subscriber(producer)` next to
  `with_publisher(&producer)`.

Public API: breaking on moq-net and moq-tokio, so on dev. Wire: none.
Consumers: everything that spawns an origin; `just check` across the
workspace plus moq.pro's next pin.

## Related

- [PathPrefixes](/quest/m1/api-path-prefixes.md) - the scope adapter PR #3746 retires
- [Origin narrowing](/quest/m2/origin-narrowing.md) - a live handle narrowing after this settles the static shape
