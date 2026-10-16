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
that predates the stream. Node-bound peer grants for direct sessions belong to
[P2P](/quest/m2/p2p/peer-grant.md), their only consumer.

## Plan

Decisions settled while planning, recorded so review does not relitigate them:

- ✅ Decided 2026-10-15 (maintainer, #5148): **one kind of auth.** A token
  is presented, verified by the acceptor through `requests()`, and earns a
  grant with a lifetime. An AUTH stream's token lives for the session and
  its grant joins the session scope. A moq-transport `AUTHORIZATION TOKEN`
  on a request is the same kind of auth, named by that request: it admits
  only that request, never joins the scope, and ends with it. Renewal on
  SUBSCRIBE_UPDATE or REQUEST_UPDATE names a new auth that replaces the old.
  Per-request tokens are moq-transport's naive form and are not surfaced as
  a separate API.
- ✅ Decided 2026-10-15 (maintainer, #5148): an unnamed request is admitted
  by the union of session-lifetime auths only. A named request outside its
  auth's grant is refused, and ends when that auth ends.
- ✅ Decided 2026-10-15 (maintainer, #5148): the work lands in steps.
  Step 1 (#5148) is incoming only: request tokens arrive on `requests()`,
  named requests are held to their issued grant, renewal is renaming, and
  no lite wire changes. Step 2 specs `auth=id` in the wip lite version so a
  lite request names an AUTH stream instead of carrying a token. Step 3
  adds outgoing request tokens through `add()` with automatic naming, sent
  as `USE_VALUE` on plain moq-transport.
- ✅ Decided 2026-10-15 (maintainer, #5148): `auth::Handle::authorize`
  keeps its name and now means the connection credential's grant; the
  session adds every grant it issued to an AUTH token itself.
- ✅ Decided 2026-10-15 (maintainer, #5148): a typed (CAT etc.) or empty
  request token is refused `NOT_SUPPORTED`. An AUTH token has no type, and
  an empty one means the connection's own credential.
- ✅ Decided 2026-10-15 (maintainer, #5148): moq-relay verifies request
  tokens in band like any token; ending a request token's lease with its
  request at the auth server stays in
  [Request leases](/quest/m3/auth-request-lease.md).
- ✅ Decided 2026-10-15 (maintainer, #5148): no cap on pending tokens. The
  peer's credit bounds them: each AUTH token holds its stream open, and each
  request token its request's slot, until answered or withdrawn.
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
  scoped grant because the caller is another relay. Mesh dials use the default
  version set, which leaves out `moq-lite-07-wip`, so this applies once lite-07
  is cut or a cluster opts in.
- **Client API.** Tokens live on `moq_tokio::connect::Config`, the
  dial-side config. `Connection::auth()` is a handle the connection owns: it
  keeps every added token, presents them on each session as it reconnects,
  and reports the live session's grant.
- **Spec home.** The AUTH stream is core in the wip lite version
  (`moq-lite-07-wip` today) in `drafts/draft-lcurley-moq-lite.md`, the way
  routing is. moq-transport gets
  `drafts/draft-lcurley-moq-auth.md`, a setup-option-negotiated extension with
  AUTH, AUTH_OK, and AUTH_ERROR control messages, mirroring how moq-cluster is
  the IETF binding of lite's routing.
- **Naming.** Messages are AUTH / AUTH_OK / AUTH_ERROR like SUBSCRIBE /
  SUBSCRIBE_OK. Rust is `moq_net::auth` with `auth::Grant`, `auth::Handle`,
  `auth::Token`, and `auth::Request`; JS mirrors as `connection.auth`.

`Session::auth()` is additive. The relay derives the grant
from the origin handles it already scopes. Decided 2026-10-05: wire
work targets the wip lite version until the maintainer cuts it, never a
published version in place, so AUTH lands in `moq-lite-07-wip` (or whichever
version is wip then), not lite-06. The UNAUTHORIZED stream code stays on lite-06
(#5004), since a relay can narrow a session with no Auth Stream; it is 0x3B,
because `main` assigned 0x3A to NOT_FETCHABLE first.

## Required

- [Live requests follow their auth](/quest/m1/auth/request-update.md) - a route or subscription swaps its auth and the session sends the update, keeping the request up
- [Requests name their auth](/quest/m1/auth/request-auth-id.md) - lite-07 AUTH carries an ID and each request names the auth that admits it
- [Request gate](/quest/m1/auth/request-gate.md) - every incoming request is
  refused up front and held to the grant through a gate its dispatcher owns
- [Bindings](/quest/m1/auth/bindings.md) - grants and tokens reach every
  binding through moq-ffi
- [Token in band](/quest/m1/auth/token-in-band.md) - the credential can leave
  the URL: a session starts on what the URL carried and its AUTH streams add
  the rest, with the URL kept for peers without the AUTH stream
  subscription even while its joining FETCH waits for request-ID credit

## Related

- [Request leases](/quest/m3/auth-request-lease.md) - deferred auth-server
  accounting that ends a request token's lease with its request
- [Request tokens beyond Rust](/quest/m3/request-token-surfaces.md) - JS, moq-ffi,
  and moq-cli request-token APIs, when a consumer needs one
- [Token on FETCH](/quest/m3/auth-token-fetch.md) - TRACK_STATUS and FETCH carry
  and honor a request token. Parked until the maintainer decides whether to
  reverse the standalone-FETCH decision and bring it into this epic
- [#5107](https://github.com/moq-dev/moq/pull/5107) - landed request-token
  decoding, including session-fatal REQUEST_UPDATE decode failures
- [#5106](https://github.com/moq-dev/moq/pull/5106) - landed setup extension
  configuration
- [Malformed grant](https://github.com/moq-dev/moq/pull/5256) - a malformed or
  non-canonical grant pattern, or an out-of-range `Expires`, closes the
  session with PROTOCOL_VIOLATION in Rust and JS
- [JS fetch grant watch](https://github.com/moq-dev/moq/pull/5260) - a JS `fetchGroup` ends `Unauthorized` when its path leaves the grant

- [Peer grants](/quest/m2/p2p/peer-grant.md) - P2P's node-bound credential,
  built on this line's relay tokens

- [#5269](https://github.com/moq-dev/moq/pull/5269) - explicit Lite AUTH NOT_SUPPORTED, including grant updates
