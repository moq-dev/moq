# In-band auth

## Goal

A session tells its peer what that peer may publish and subscribe to, and a
peer can present credentials without reconnecting. Today a publisher whose
token allows `baz` but who publishes `foo/bar` waits forever: the relay only
solicits announcements for the granted prefixes, nothing is written or logged
on either side, and moq-lite has no announce refusal at all. Tokens ride the
URL, so a session can never outlive its credential and a client holding two
tokens needs two connections. On-demand publishing needs the missing half of
this: a publisher must know it is authorized before demand arrives, so silence
means "nobody wants it yet" instead of "nobody ever will".

This questline adds an AUTH exchange to both wires: one stream per token, a
grant per token, the union of every accepted token as the session's scope,
and a loud failure when a publish can never be honored. It ends with the
credential able to travel in band, while the URL keeps working for every peer
that predates the stream.

## Plan

Decisions settled while planning, recorded so review does not relitigate them:

- **One AUTH stream per token, opened by whoever presents it.** Either side of
  a session may open one; the acceptor answers with the grant that token
  earns. A moq-net session opens one on both sides right after SETUP with an
  empty token, meaning the credential the connection already presented (URL
  `?jwt=`, mTLS, or nothing), so a publish-only client learns it may subscribe
  to nothing and a relay learns which role its peer will ever play. This is
  also the answer to
  [moq-wg #1854](https://github.com/moq-wg/moq-transport/issues/1854): the
  grant names the peer's role in transport.
- **Tokens union.** A client with two tokens opens two streams and the
  session's scope is the union of both grants. A refresh is a new stream
  carrying the new token; nothing replaces or narrows anything. Closing a
  stream withdraws that token. When a token expires or is revoked and the
  union would shrink, the session closes `Unauthorized` exactly as expiry does
  today; when the union is unchanged the session continues and only that
  stream ends. Resizing a live session in place belongs to
  [Relay auth](/quest/m2/path-patterns/relay-auth.md), which serves
  revalidation and token expiry from one path.
- **A grant is publish prefixes, subscribe prefixes, and an expiry**, in the
  presenter's own root; the presenter never sees the relay-side root, and
  every token in a union shares the connection's root. The prefix encoding is
  whatever lite-06 ANNOUNCE_REQUEST carries, so
  [Pattern interest](/quest/m2/path-patterns/interest.md) upgrades both to
  patterns in one change. `origin::Producer::allowed()` on an unscoped handle
  yields the single empty prefix, and `scope(&[])` is `None`, so the wire
  keeps that spelling: a list holding `""` is everything, an empty list is
  nothing.
- **Fail loud by aborting the session.** A publisher whose origin announces a
  broadcast outside the union aborts the session with `Unauthorized`, naming
  the path. The check runs against the grants in hand once the tokens the
  library itself presented at setup are answered; a token the app adds later
  is the app's to await before publishing what it unlocks. The origin is
  untouched, so a broadcast shared across sessions (P2P hops, a cluster) is
  refused only where it is refused. Apps that want to decide themselves read
  the grant instead.
- **Older peers keep the URL.** WebTransport negotiates the moq version as a
  subprotocol of the same CONNECT request that carries the URL, so a client
  cannot learn whether the peer speaks AUTH before the token has to be sent.
  The client puts the connection credential in the URL as long as any
  version it offers has no AUTH stream and omits it once the offered set is
  AUTH-capable; extra tokens go in band and are simply absent on an old peer.
  Nothing is refused and nothing is silent.
- **Cluster peers keep mTLS.** A relay opens AUTH toward its peer with an empty
  token like any client; both directions learn the unrestricted grant. Mutual
  scoped trust between relays is a later quest.
- **Client API is dev's.** Tokens live on `moq_tokio::connect::Config`, the
  dial-side config already on dev, and `Connection` exposes the live
  session's auth handle. Quests touching that surface branch from dev, or
  from main once [merge-dev](/quest/m1/merge-dev.md) lands.
- **Spec home.** The AUTH stream is lite-06 core in
  `drafts/draft-lcurley-moq-lite.md`, the way routing is. moq-transport gets
  `drafts/draft-lcurley-moq-auth.md`, a setup-option-negotiated extension with
  AUTH, AUTH_OK, and AUTH_ERROR control messages, mirroring how moq-cluster is
  the IETF binding of lite's routing.
- **Naming.** Messages are AUTH / AUTH_OK / AUTH_ERROR like SUBSCRIBE /
  SUBSCRIBE_OK. Rust is `moq_net::auth` with `auth::Grant`, `auth::Handle`,
  `auth::Token`, and `auth::Request`; JS mirrors as `connection.auth`.

Everything here is additive: `Session::auth()` is new, the relay derives the
grant from the origin handles it already scopes, and lite-06 is an opt-in WIP
ALPN.

## Quests

- [Lite stream](/quest/m2/auth/lite.md) - both sides of a lite-06 session
  exchange grants over AUTH streams, exposed as `Session::auth()`, and an
  out-of-scope announce aborts the session
- [Relay tokens](/quest/m2/auth/relay-refresh.md) - the relay verifies tokens
  sent in band, unions their grants, and closes only when an expiry shrinks
  the union
- [moq-transport](/quest/m2/auth/moq-transport.md) - the same exchange as a
  setup-option extension on draft-17+, specified in a new draft
- [Bindings](/quest/m2/auth/bindings.md) - grants and tokens reach every
  binding through moq-ffi and libmoq
- [Token in band](/quest/m2/auth/token-in-band.md) - the credential can leave
  the URL: a session starts on what the URL carried and its AUTH streams add
  the rest, with the URL kept for peers below lite-06

## Related

- [Relay auth](/quest/m2/path-patterns/relay-auth.md) - resizes a live session
  when the union shrinks, for revalidation and token expiry alike
- [Pattern interest](/quest/m2/path-patterns/interest.md) - moves the grant's
  prefixes to patterns along with ANNOUNCE_REQUEST
- [Expiring media grants](/quest/m2/processor/grant-lease.md) - a worker's
  lease renewal is a new in-band token
- [Connect auth race](/quest/m0/3532-connect-auth-race.md) - the connect-time
  auth error this questline does not change
