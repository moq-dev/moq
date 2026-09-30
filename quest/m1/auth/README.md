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
that predates the stream. Hop-bound peer grants for direct sessions belong to
[P2P](/quest/m2/p2p/peer-grant.md), their only consumer.

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
  session's scope is the union of both grants. Refresh adds a new token;
  closing a stream withdraws it. Expiry, revocation, or withdrawal recomputes
  the union and cancels publications and subscriptions that lose authorization.
  Other authorized work continues on the same session. An empty union leaves
  the session connected with no access, so it can accept a fresh token.
  `auth::Handle::authorize` is the common resize operation; relay token handling
  uses it rather than shipping a temporary close-on-shrink policy.
- **A public grant contains publish patterns, subscribe patterns, and an
  expiry**, in the presenter's own root; the presenter never sees the relay-side
  root, and every token in a union shares the connection's root. Unscoped
  permission is `**`; an empty union grants nothing. Lite AUTH_OK carries
  those patterns, wildcards and literals alike, from the first release, with
  no covering-prefix workaround. IETF AUTH_OK carries Track Namespace
  prefixes, so an acceptor whose grant is not a union of subtrees answers
  AUTH_ERROR NOT_SUPPORTED rather than widening it. Announce stays a prefix:
  ANNOUNCE_REQUEST and SUBSCRIBE_NAMESPACE do not gain patterns. The public
  grant type stays pattern-valued.
- **Fail loud by aborting the session.** A publisher whose origin announces a
  broadcast outside the union aborts the session with `Unauthorized`, naming
  the path. The check runs against the grants in hand once the tokens the
  library itself presented at setup are answered; a token the app adds later
  is the app's to await before publishing what it unlocks. The origin is
  untouched, so a broadcast shared across sessions (P2P hops, a cluster) is
  refused only where it is refused. Grant shrinkage cancels previously
  authorized work on this session without aborting it or deleting the shared
  origin; this is distinct from attempting a new unauthorized publication.
  Apps that want to decide themselves read the grant instead.
- **Older peers keep the URL.** WebTransport negotiates the moq version as a
  subprotocol of the same CONNECT request that carries the URL, so a client
  cannot learn whether the peer speaks AUTH before the token has to be sent.
  The client puts the connection credential in the URL as long as any
  version it offers has no AUTH stream and omits it once the offered set is
  AUTH-capable; extra tokens go in band and are simply absent on an old peer.
  Nothing is refused and nothing is silent.
- **Cluster peers keep mTLS.** A relay opens AUTH toward its peer with an empty
  token like any client; AUTH reports the grant actually admitted in each
  direction. The next mTLS scope quest can restrict or refuse it. A v1 endpoint
  must explicitly grant `**` for unrestricted access; AUTH does not widen a
  scoped grant because the caller is another relay.
- **Client API.** Tokens live on `moq_tokio::connect::Config`, the
  dial-side config. `Connection::auth()` is a handle the connection owns: it
  keeps every added token, presents them on each session as it reconnects,
  and reports the live session's grant.
- **Spec home.** The AUTH stream is lite-06 core in
  `drafts/draft-lcurley-moq-lite.md`, the way routing is. moq-transport gets
  `drafts/draft-lcurley-moq-auth.md`, a setup-option-negotiated extension with
  AUTH, AUTH_OK, and AUTH_ERROR control messages, mirroring how moq-cluster is
  the IETF binding of lite's routing.
- **Naming.** Messages are AUTH / AUTH_OK / AUTH_ERROR like SUBSCRIBE /
  SUBSCRIBE_OK. Rust is `moq_net::auth` with `auth::Grant`, `auth::Handle`,
  `auth::Token`, and `auth::Request`; JS mirrors as `connection.auth`.

Everything here is additive: `Session::auth()` is new, the relay derives the
grant from the origin handles it already scopes, and AUTH is added to the
existing lite-06 ALPN.

## Required

- [Lite NOT_SUPPORTED](/quest/m1/auth/not-supported.md) - a lite acceptor answers AUTH_ERROR NOT_SUPPORTED after a grant too, and lite messages cap at moq-transport's 65,535 bytes
- [AUTH violations](/quest/m1/auth/violations.md) - every AUTH protocol violation closes the session in Rust and JS, lite and IETF
- [JS fetch grant watch](/quest/m1/auth/js-fetch-watch.md) - a JS `fetchGroup` ends `Unauthorized` when its path leaves the grant
- [Relay tokens](/quest/m1/auth/relay-refresh.md) - the relay verifies tokens
  sent in band, unions their grants, and cancels only work that loses access
- [Request tokens](/quest/m1/auth/request-token.md) - an `AUTHORIZATION
  TOKEN` on a moq-transport request authorizes that request when the session
  grant does not, and REQUEST_UPDATE refreshes it
- [Expired token error](/quest/m1/auth/expired-error.md) - an expired token
  reports `Error::Expired`, not `Unauthorized`, in Rust, JS, and the bindings
- [Bindings](/quest/m1/auth/bindings.md) - grants and tokens reach every
  binding through moq-ffi
- [Token in band](/quest/m1/auth/token-in-band.md) - the credential can leave
  the URL: a session starts on what the URL carried and its AUTH streams add
  the rest, with the URL kept for peers below lite-06

## Related

- [Expiring media grants](/quest/m2/processor/grant-lease.md) - a worker's
  lease renewal is a new in-band token
- [Peer grants](/quest/m2/p2p/peer-grant.md) - P2P's hop-bound credential,
  built on this line's relay tokens
