# [M] Relay tokens

## Goal

A client presents tokens in band and moq-relay verifies each through the same
`Auth` path the URL token took: the session's scope is the union of every
accepted token, a refused token gets AUTH_ERROR and changes nothing, and a
session whose credential is about to lapse renews by presenting a fresh one
before the old expires. The grant a client receives carries the token's real
expiry, an expiry that leaves the union intact ends only that token, and
`doc/bin/relay/auth.md` documents the exchange.

## Plan

- `Connection::run` in `rs/moq-relay/src/connection.rs` takes `requests()`
  from `moq_net::server::Handshake::auth()` before `.ok()`, so the relay owns
  the initial empty AUTH too and the driver's fallback never races it.
  `moq_tokio::server::Request::ok` spawns the driver itself, so it needs the
  same `auth()` passthrough first. An empty
  token is answered from the origin handles as the default does, plus
  the admitted grant's `expires`. A non-empty token is presented through
  `Client::attach(&connection_lease, request)`, a method this quest adds:
  the request carries the connection's `id`, path, and transport, the query
  `jwt=<token>` exactly as the URL would have, and no certificate facts,
  since a certificate cannot be presented in band. An `attach` lease
  revalidates like any other but never POSTs `end`, and the server treats a
  `connect` for an id it already holds as one more grant on that session,
  so lifecycle stays with the connection lease and a withdrawn token frees
  no session-limit slot. `doc/bin/relay/auth.md` states that rule. mTLS
  sessions reject a non-empty token as `Unsupported`.
- Union: the connection holds the set of accepted leases. Every one
  must carry the admitted root, else `AUTH_ERROR { Unauthorized }` naming the
  root. The peer is authorized for the union of the set's publish and subscribe
  patterns, intersected with the role the client declared at SETUP so a
  publish-only session never starts receiving announcements because a later
  token happened to carry subscribe prefixes, re-authorized through
  `auth::Handle::authorize` whenever the union changes, grown or shrunk; keep it
  behind one function. A grant never reaches past the session's origin handles,
  so scope those to the admitted root and role rather than to the first
  token's patterns, and nothing is rebuilt or swapped in.
- Expiry: the deadline today is the admitted lease's `closed()` inside one
  `tokio::select!` arm. Select on one lease per token in the set instead, and on any firing recompute the union without it: unchanged
  means `AUTH_ERROR { Expired }` on that token's stream and the session
  continues, shrunk re-authorizes the smaller union, which resets each stream that
  lost access with `UNAUTHORIZED`. A token
  withdrawn by the client (its stream closed or reset) recomputes the union
  the same way with nothing written back, since the stream is gone: unchanged
  means the session continues, shrunk re-authorizes the same way. Each lease revalidates on
  its own cadence, and every re-check that changes the grant, a lower
  `expires` or a different `publish` or `subscribe`, writes the complete
  replacement grant as an update AUTH_OK on that token's stream and
  recomputes the union from the latest grant of every token, so the client
  can present a replacement in time and a narrowed token never keeps its old
  scope; a changed union re-authorizes as above.
- Refusals map as connect-time ones do and never expose a status: every
  refusal `Client::attach` returns for a new token, a `403`, an empty or
  invalid grant, an unparseable body, or a `5xx`, is
  `AUTH_ERROR { Unauthorized }`, and a timeout is `AUTH_ERROR { Timeout }`.
  On re-check a refusal closes that token's stream with
  `AUTH_ERROR { Unauthorized }` and recomputes the union, while a failed
  re-check (timeout or `5xx`) keeps the token until its `expires`, never a
  close on its own.
- The client side: `moq_tokio::Connection` gains `auth()`
  returning a handle the connection owns, not the current session's. It
  stores every token added through it, presents them on each new session as
  it attaches, unions the live session's grant, and its `add` resolves against
  the session that is up at the time; a token the app drops is withdrawn from
  the live session and forgotten. Its request-token half belongs to
  [Request tokens](/quest/m1/auth/request-token.md); whichever lands first
  adds the handle.
- Bound pending tokens: `auth::Handle::requests()` hands out an unbounded
  queue today, and a closed AUTH stream leaves its request queued, so a peer
  that finishes AUTH streams faster than the relay verifies them grows it
  without limit (Codex security review on #4039). This quest is the first
  consumer, so bound it per session here and refuse what overflows; dropping a
  `Request` already refuses its token.
- Docs: `doc/bin/relay/auth.md` gains an "in-band tokens" section beside
  revalidation stating that grants union, that a token needs the admitted
  root, what an expiry does to the union today, and that the grant's expiry
  is the token's `exp`.
- Tests: a second token with a later `exp` and the same scope keeps the
  session past the first token's expiry, with only the first stream ending;
  an expired or invalid token is refused and nothing changes; a token with a
  different root is refused naming it; a token adding a prefix widens the
  grant and the prefix is now solicited; withdrawing the only token
  covering a prefix resets that prefix's streams with `UNAUTHORIZED` and
  the session stays connected; an mTLS session refuses a token; a
  proxy-mode outage on re-check keeps the token.

Additive.

**Implementation and remaining decisions:**

The branch now verifies in-band tokens through the relay admission path, holds
independent credentials behind one authorization union, and preserves the
connection's identity and End ownership across attached leases. Network origin
handles cover the admitted root and role, so a newly granted prefix can become
reachable without rebuilding the session. Native, HTTP WebSocket, and io_uring
admission take AUTH before starting the protocol driver. The persistent native
Auth handle re-presents held tokens after replacement and keeps the predecessor's
grant alive while it drains. The pending request queue is bounded in Rust and JS.

Keep this quest until the public choices and wire error fidelity below settle:

- Persistent API: recommend the planned `Connection::auth()` with `Auth`, `Token`,
  and `Watch`, adding request tokens through the same handle in #5148. Alternatively
  land #5148 first and rebase this implementation onto its handle. Do not expose two
  competing handles or silently discard one kind of token on reconnect.
- Admission ownership: recommend `Client::attach(&Consumer, Request)` with a
  cloneable Consumer and consuming `close(self)` ending all clones. A separate public
  connection-admission handle would make ownership explicit at the cost of another
  type and changes to every admission caller. Internal identity checks already
  reject another client, a non-owning attachment, or changed connection facts.
- Pending work: recommend a fixed 128 queued requests plus 128 verifications per
  session. An explicit configurable limit is the alternative; unbounded work is
  unsafe. Withdrawn requests release queued space, but an already-started
  verification must retain its slot until it finishes.
- Wire errors: the current cross-protocol API uses `SessionError` for AUTH refusals.
  It has no expired-credential variant and IETF currently collapses all refusals
  except unsupported into UNAUTHORIZED. This implementation preserves that mapping,
  with the expiry/refusal reason in the reason phrase; it does not yet meet this
  plan's distinct Expired and Timeout results. Recommend a focused typed AUTH
  error API shared by Rust and JS, mapped to each protocol's existing registry,
  instead of inventing session codes. Alternatively retain the coarse API and
  revise the goal explicitly. No such decision is presumed approved.
- Stats tier: recommend that the original connection credential owns tier updates;
  attached credentials contribute permissions only. A deterministic tier-selection
  policy across credentials is an alternative, requiring an explicit policy rule.

The auth server already treats a known connection id as one live session. This
branch preserves that connection-level accounting instead of changing limits to
count individual tokens. The original lease alone reports End, after any
attachment admission already in flight, even if its credential expired earlier.
