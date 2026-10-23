# [L] Auth outlives its session

## Goal

A route on a reconnecting connection names its token once: the same
`moq_net::auth::Auth` keeps riding its announcements on every session the
connection makes, with no re-add or re-announce from the application.

## Plan

- Today a `moq_net::auth::Auth` belongs to the session whose `auth::Handle`
  added it, and `Route::auth` carries it only on that session, so after a
  reconnect the application adds the token again on the new session and
  re-announces. `moq_tokio::Connection::auth().add()` keeps a token across
  sessions, but it returns moq-tokio's own `connection::auth::Auth`, which a
  `Route` cannot name.
- ✅ Decided 2026-10-16 (maintainer, #5695): the long-term shape is a
  session-independent issuer in moq-net. An `auth::Handle` outlives sessions
  and is passed in with something like `Client::with_auth(handle)` (and the
  `Server` equivalent). Each session built on it presents every token the
  handle holds, and a `moq_net::auth::Auth` it issued is carried by every
  session of that handle. This revisits the "bound to the handle that issued
  it" decision of [#5695](https://github.com/moq-dev/moq/pull/5695): the
  binding stays, but the handle spans sessions.
- moq-tokio's replay loop moves into moq-net. Today
  `rs/moq-tokio/src/connection/auth.rs` spawns a task per added token that
  presents it on each new session (`present`, with backoff on a transient
  refusal and a final stop on `Unauthorized`). With the handle in moq-net,
  `Connection::auth()` returns that `moq_net::auth::Handle`, and
  `connection::auth::{Handle, Auth}` go away.
- Decided 2026-10-20 (maintainer): a handle backs one active session at a
  time, plus the predecessors it is draining, matching the reconnect code's
  single active generation. Attaching a handle to a second concurrent session
  fails loudly; independent concurrent sessions use separate handles, so an
  `Auth` never has to pick between two live grants.
- An `Auth`'s `grant()` and `closed()` follow the session that is live now: the
  grant clears while disconnected and returns once the new session's peer
  answers, and `closed()` settles only on a refusal or when the handle ends.
- The ietf publisher's check "presented on this session" (the route's `Auth`
  was issued by this session's handle) keeps working unchanged once the handle
  is shared, so a route survives a reconnect with no re-announce from the app.
- Mirror in `js/net`: the JS `Handle` lives on the reconnecting connection
  rather than one session, and `Route.auth` follows it.
- Update `doc/concept/moq-lite.md`, `doc/concept/standard.md` and
  `doc/lib/js/net.md`, which today say a reconnect means adding the token
  again and re-announcing.

Public API: breaking in moq-net (`Handle` construction and the `Client`/`Server`
builders), moq-tokio (the `connection::auth` types go away), and js/net. Wire:
none.

## Related

- [#5695](https://github.com/moq-dev/moq/pull/5695) - adds `Route::auth`, bound to one session's handle
- `rs/moq-ffi/src/auth.rs` - wraps both the session and the connection handle behind one `MoqAuth`; collapse it onto this shape
