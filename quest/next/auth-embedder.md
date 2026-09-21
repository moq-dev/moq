# [M] An embedder admits a gateway session with the relay's own loop

## Goal

An in-process decider and a gateway session get what the HTTP path and a
QUIC session get for free: the lease owns its re-check clock, a gateway
session holds a lease so revocation reaches RTMP, SRT, WHIP, and WHEP, and
the relay scopes and tags the origins from the grant in one call. moq.pro's
`rs/edge/src/gateway.rs` re-implements `Connection::run`'s authorize, scope,
and stats-tier step for four gateways and holds no lease for any of them.

## Plan

Additive on `moq-auth` and `moq-relay`, so on main after the merge:

- `lease::Producer::new(grant)` records `revalidate`/`expires`;
  `producer.due().await -> Due::{Revalidate, Expired}`, `update(grant)`
  reschedules, `failed()` applies the backoff. `moq_auth::Client` becomes a
  ten-line loop over it, and an embedder answering `Admissions` writes the
  same ten lines instead of a second driver.
- `Cluster::admit(&self, auth: &Auth, request: moq_auth::Request) ->
  Result<Admitted, auth::Error>` with
  `Admitted { lease: Lease, publisher: Option<origin::Producer>, subscriber: Option<origin::Consumer>, stats: stats::Session }`
  already scoped and tagged from `grant.tier`; today `authorize` and
  `Grants` are `pub(crate)` in `rs/moq-relay/src/connection.rs` and
  `Cluster::publisher(&Token)` drops the `Lease`. `auth::hold(lease, work)`
  holds non-session work for as long as the lease allows, the way
  `supervise` does for a `moq_net::Session`.
- `moq_auth::Transport::{Rtmp, Srt, WebRtc}` so gateway sessions produce
  `connect`/`end` events and count toward `serve::Limits`; the `@moq/auth`
  `TransportSchema` is a strict enum, so it gains the three values with
  interop coverage in the same PR.
- `Key::decode<C: DeserializeOwned>(token) -> Result<C>` (signature,
  algorithm pinning, `kid`, nothing else) with `verify` built on it, and
  `decode()` beside `verify()` in `@moq/auth`; `Claims` refuses unknown
  fields on purpose, so moq.pro keeps a second JWT decoder in each language
  for its migration window.
- `lease::Reason::{Narrowed, Shutdown}` and the relay's own spellings in
  `doc/bin/relay/auth.md`; the `end.reason` vocabulary is an open string
  today.
- `serve::Keys::Set(PathBuf)` reading a JWKS through `KeySet::verify`, so a
  key file, a directory, and a set all enforce `kid`.

Public API: additive. Wire: the auth JSON gains transport values and end
reasons.

## Related

- [Relay embedding](/quest/next/relay-embed.md) - the rest of the embedder surface
- [Stats retier](/quest/next/stats-retier.md) - what a re-checked tier does to the `stats::Session` handed out here
