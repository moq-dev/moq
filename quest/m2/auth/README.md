# In-band auth

## Goal

A session tells its peer what that peer may publish and subscribe to, and a
peer can present a new credential without reconnecting. Today a publisher whose
token allows `baz` but who publishes `foo/bar` waits forever: the relay only
solicits announcements for the granted prefixes, nothing is written or logged
on either side, and moq-lite has no announce refusal at all. Tokens ride the
URL, so a session can never outlive its credential. On-demand publishing needs
the missing half of this: a publisher must know it is authorized before demand
arrives, so silence means "nobody wants it yet" instead of "nobody ever will".

This questline adds an AUTH exchange to both wires: a grant after setup, a
refresh whenever the presenter chooses, and a loud failure when a publish can
never be honored. It ends with the credential leaving the URL for a session
that starts anonymous and widens in band.

## Plan

Decisions settled while planning, recorded so review does not relitigate them:

- **Symmetric streams, opened by whoever wants a grant.** Either side of a
  session may open an AUTH stream; the acceptor answers with the grant for the
  opener. A moq-net session opens one on both sides right after SETUP, whether
  or not it presented a credential, so a publish-only client learns it may
  subscribe to nothing and a relay learns which role its peer will ever play.
  This is also the answer to
  [moq-wg #1854](https://github.com/moq-wg/moq-transport/issues/1854): the
  grant names the peer's role in transport.
- **Every grant answers a token.** The opener's first message is always AUTH,
  and an empty token means "the credential this connection already presented":
  URL `?jwt=`, mTLS, or nothing. A later real token on the same stream is a
  refresh, and once the relay admits anonymous sessions and widens on AUTH, it
  is also how the initial credential arrives in band.
- **A grant is publish prefixes, subscribe prefixes, and an expiry**, in the
  presenter's own root; the presenter never sees the relay-side root. The
  prefix encoding is whatever lite-06 ANNOUNCE_REQUEST carries, so
  [Pattern interest](/quest/m2/path-patterns/interest.md) upgrades both to
  patterns in one change. `origin::Producer::allowed()` on an unscoped handle
  yields the single empty prefix, and `scope(&[])` is `None`, so the wire
  keeps that spelling: a list holding `""` is everything, an empty list is
  nothing.
- **Fail loud by aborting the session.** A publisher whose origin announces a
  broadcast outside `grant.publish` aborts the session with `Unauthorized`,
  naming the path, once its outstanding AUTHs are answered. The origin is
  untouched, so a broadcast shared across sessions (P2P hops, a cluster) is
  refused only where it is refused. Apps that want to decide themselves read
  the grant instead.
- **Narrowing is refused, for now.** A refresh whose grant does not cover the
  current one gets AUTH_ERROR and the session keeps running on the old grant
  until its expiry, matching what revalidation does today. In-place resizing
  belongs to [Relay auth](/quest/m2/path-patterns/relay-auth.md), which
  already plans it for revalidation and serves both from one path.
- **Cluster peers keep mTLS.** A relay opens AUTH toward its peer with an empty
  token like any client; both directions learn the unrestricted grant. Mutual
  scoped trust between relays is a later quest.
- **Spec home.** The AUTH stream is lite-06 core in
  `drafts/draft-lcurley-moq-lite.md`, the way routing is. moq-transport gets
  `drafts/draft-lcurley-moq-auth.md`, a setup-option-negotiated extension with
  AUTH, AUTH_OK, and AUTH_ERROR control messages, mirroring how moq-cluster is
  the IETF binding of lite's routing.
- **Naming.** Messages are AUTH / AUTH_OK / AUTH_ERROR like SUBSCRIBE /
  SUBSCRIBE_OK. Rust is `moq_net::auth` with `auth::Grant`, `auth::Handle`,
  and `auth::Request`; JS mirrors as `connection.auth`.

Everything here is additive on main: `Session::auth()` is new, the relay
derives the grant from the origin handles it already scopes, and lite-06 is an
opt-in WIP ALPN.

## Quests

- [Lite stream](/quest/m2/auth/lite.md) - both sides of a lite-06 session
  exchange grants over an AUTH stream, exposed as `Session::auth()`, and an
  out-of-scope announce aborts the session
- [Relay refresh](/quest/m2/auth/relay-refresh.md) - the relay verifies a token
  sent in band, replaces the session's expiry, and refuses a narrowing one
- [moq-transport](/quest/m2/auth/moq-transport.md) - the same exchange as a
  setup-option extension on draft-17+, specified in a new draft
- [Bindings](/quest/m2/auth/bindings.md) - grant and refresh reach every
  binding through moq-ffi and libmoq
- [Token in band](/quest/m2/auth/token-in-band.md) - the credential leaves the
  URL: a session starts anonymous and its first AUTH widens it

## Related

- [Relay auth](/quest/m2/path-patterns/relay-auth.md) - resizes a live session
  on a narrower grant, for revalidation and in-band refresh alike
- [Pattern interest](/quest/m2/path-patterns/interest.md) - moves the grant's
  prefixes to patterns along with ANNOUNCE_REQUEST
- [Expiring media grants](/quest/m2/processor/grant-lease.md) - a worker's
  lease renewal is an in-band refresh
- [Connect auth race](/quest/m0/3532-connect-auth-race.md) - the connect-time
  auth error this questline does not change
