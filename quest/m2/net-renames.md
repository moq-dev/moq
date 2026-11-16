# [S] moq-net's node, version, handshake, and route priority carry their wire names

## Goal

Four public moq-net names match what they name before the cut publishes them:

- An origin's identity is its `node`: `origin::Config::node`,
  `origin::Producer::node()`, and `origin::Consumer::node()` replace `hop`,
  matching the Node ID the lite-07 wire and SETUP carry.
- `moq_net::Version` is an opaque type. A caller parses it, prints it, and
  asks `is_lite()`, `alpn()`, and `as_str()`; no variant is public, so adding
  or dropping a draft is never an API change.
- `Server::handshake(now, session)` replaces `Server::accept_request`.
- A route's rank is its `priority`, as the Route Offer's byte is named:
  `origin::Producer::with_priority` and `origin::Priority` replace
  `with_preference` and `Preference`.

Each rename mirrors into `@moq/net` and the bindings wherever the name exists
there.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ `origin::Config::hop` becomes `node` (rejected: keeping `hop`). Hop
  chains went private in the window, so `hop` is a word with no public
  referent, and the wire says Node ID
  (`rs/moq-net/src/lite/setup.rs`).
- ✅ `Version` becomes opaque (rejected: keeping the enum). Its variants hold
  `lite::Version` and `ietf::Version`, and both modules are private
  (`rs/moq-net/src/lib.rs:107,110`), so no caller can build or match a draft.
- ✅ `Server::accept_request` becomes `handshake` (rejected: keeping it). It
  is named for a `Request` type that no longer exists and returns an `Accept`
  that yields a `Handshake`.
- ✅ Route `with_preference` becomes `priority` (rejected: making it private
  until federation needs it; keeping the name). The API says preference
  while the wire and the draft say priority.

Facts found by the audit, checked at `80ca9787e3`:

- `hop` is public at `rs/moq-net/src/model/origin.rs:107` (the field),
  `:2459`, and `:7483` (the accessors). `Client::with_peer_hop` and
  `peer_hop` name the peer's identity the same way and are read by moq-tokio
  (`rs/moq-tokio/src/connection.rs:967`) and moq-relay
  (`rs/moq-relay/src/incoming.rs:88`). `origin::Config::max_hops` counts the
  hops of a path and keeps its name;
  [the route switch](/quest/m2/multicast/switch.md) marks it legacy.
- The one match on a `Version` variant outside moq-net is a test
  (`rs/moq-relay/tests/integration/runtime_uring.rs:436`), which
  `is_lite()` replaces. `rs/moq-net/AGENTS.md` already says to branch on
  `is_lite()` and `alpn()` outside the crate. The relay also calls the
  crate-level `Version::has_request_path` (`rs/moq-relay/src/cluster.rs:1779`,
  `:2334`), which stays.
- `accept_request` (`rs/moq-net/src/server.rs:396`) has callers in 18 files
  across moq-net, moq-relay, moq-tokio, and moq-uring.
- `with_preference` (`rs/moq-net/src/model/origin.rs:2390`) has no caller
  outside moq-net and its integration tests; `doc/lib/rs/moq-net.md:21`
  documents it for ranking several CDNs. Its planned consumer is
  [outbound federation](/quest/m3/cluster-routing/federation.md), which adds
  the relay's key for it.

For the PR:

- Rename `peer_hop` and `with_peer_hop` to `peer_node` and `with_peer_node`
  in the same pass, so one word names an identity. The relay's
  `cluster.node` key is a URL, not this number: keep them apart in docs, and
  say in the PR if the collision argues for another word.
- `Version`: keep `ALL`, `FromStr`, `Display`, and serde as they are. If a
  caller turns up that must tell one draft from another, add a method for
  that question instead of exposing the variant.
- `origin::Priority` keeps `Preference`'s range and meaning (higher wins, 0
  is neutral, negative demotes). `track::Subscription` already has a
  `priority`; say in the docs which is which.
- Update `doc/lib/rs/moq-net.md`, `doc/concept/moq-lite.md`, and each
  package's `MIGRATE.md` note through the PR's Impact section.

Public API: breaking renames in Rust (`origin::Config::hop`, the `hop()`
accessors, `peer_hop`, the `Version` variants, `Server::accept_request`,
`with_preference`, `Preference`) and wherever JS and the bindings mirror
them. Wire: none.

## Related

- [The route switch](/quest/m2/multicast/switch.md) - removes the route key beside the node identity this renames
- [Session close codes](/quest/m2/session-codes.md) - the other public moq-net cleanup in `error.rs`
- [Unused moq-net API](/quest/m2/unused-api/net.md) - makes the lite version predicates crate-private beside the opaque `Version`
