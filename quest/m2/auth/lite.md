# [L] Lite stream

## Goal

Both sides of a moq-lite-06 session learn what they may publish and subscribe
to. Right after SETUP each side opens an AUTH stream carrying an empty token
and receives its grant; `Session::auth()` exposes the union of every grant and
lets the app present more tokens, each on its own stream. A publisher whose
origin announces a broadcast outside that union aborts the session with
`Unauthorized` naming the path instead of waiting for a solicitation that
never comes. Rust and JavaScript both speak it, and the lite draft specifies
it.

## Plan

### Wire

Add stream type `0x7` AUTH, creator either, to the bidirectional stream table
in `drafts/draft-lcurley-moq-lite.md` and a section beside Probe modeled on
it. One stream carries one token for the life of that token. Messages,
encoded like PROBE with the `(i)`, `(b)`, `(s)` notation the draft uses:

- `AUTH { Token (b) }`, written once by the opener as the stream's first
  message. An empty token means the credential the connection already
  presented, or nothing.
- `AUTH_OK { Publish Count (i), Publish Prefix (s)..., Subscribe Count (i),
  Subscribe Prefix (s)..., Expires (i) }`, written by the acceptor. The
  prefixes use the encoding and rules ANNOUNCE_REQUEST's prefix uses: a list
  holding the empty prefix grants everything, an empty list grants nothing.
  Expires is milliseconds until the grant lapses, zero for never. The acceptor
  MAY write further AUTH_OK messages on the same stream to update that
  token's grant, such as a lowered expiry; the latest one is the token's grant.
- `AUTH_ERROR { Code (i), Reason (s) }`, written by the acceptor. As the
  first reply it refuses the token; after an AUTH_OK it revokes it. Either
  way the acceptor then closes the stream.

A session's scope is the union of the grants of its open AUTH streams. The
opener withdraws a token by closing or resetting its stream. A stream reset
before any reply, which is what a peer does with a stream type it does not
understand (the existing rule at the STREAM_TYPE section), leaves the opener
with no grant for that token, and the client reports it as `Unsupported`
rather than a refusal. Record it in the lite-06 changelog. Run
`just drafts check`.

### Model

New module `rs/moq-net/src/auth.rs`, public as `moq_net::auth`:

- `auth::Grant { publish: PathPrefixes, subscribe: PathPrefixes, expires:
  Option<Instant> }`, a plain value in the presenter's root.
- `auth::Handle`, cloneable, returned by `Session::auth()`. `grant()` is a
  watchable union of every open token's grant (`None` until the first
  AUTH_OK; the `kio` shared cell the session already uses for the peer SETUP
  is the pattern). `add(token) -> Result<auth::Token, Error>` opens a stream
  and resolves with the first reply. `auth::Token` exposes that token's own
  watchable `grant()` and `closed()`, and dropping it closes the stream,
  withdrawing the token. `requests()` yields `auth::Request { token,
  accept(Grant) -> auth::Issued, reject(code, reason) }` for the acceptor
  side, where `auth::Issued` can `update(Grant)` and `revoke(code, reason)`
  and its drop ends the stream; a request left unanswered is a bug, so
  dropping one rejects with `Unauthorized`.
- The default acceptor needs no app code, and whether it is in charge is
  decided once, before the driver starts, never per message: the server-side
  `moq_net::Request` builder exposes `auth()` before `.ok()`, and a caller
  that takes `requests()` there owns every AUTH the session receives, the
  initial empty one included. With no such consumer, the driver answers the
  empty token from the session's own origin handles, publish from the
  subscriber half's `origin::Producer::allowed()` and subscribe from the
  publisher half's `origin::Consumer::allowed()`, with no expiry. A missing
  half grants nothing. That is what makes the cluster case free: both relays
  present mTLS, both learn the unrestricted grant. A non-empty token with no
  consumer is rejected `Unsupported`; the relay is the consumer in
  [Relay tokens](/quest/m2/auth/relay-refresh.md).

`Session` is protocol-agnostic (`rs/moq-net/src/session.rs` holds a version,
bandwidth handles, and a boxed driver), so `lite::start` builds the handle and
hands it to `Session::new`; `ietf::start` hands one whose `grant()` stays
`None` and whose `add` fails `Unsupported` until
[moq-transport](/quest/m2/auth/moq-transport.md) fills it in.

### Session

Accepted bidi streams are dispatched by the publisher half alone
(`Publisher::handle` in `rs/moq-net/src/lite/publisher.rs`), which holds only
the publish-side `origin::Consumer`; the grant needs both halves' `allowed()`,
so the handle is built in `lite::start` with both handles and shared into the
publisher and subscriber, the way the peer-setup slot is. Opening: the
subscriber half opens the empty-token AUTH stream where it opens Probe, after
the peer SETUP resolves, and `add` opens further ones on demand. Accepting: a
new `recv_auth` beside `recv_probe` runs one stream for the life of its
token, on error aborting only that stream.

Fail loud: the publisher half already scopes its origin per solicited prefix;
add a check that every path its origin announces is covered by the union's
publish prefixes, run when a grant arrives or changes and when a broadcast is
announced. It waits only for the tokens the session presented itself at setup
(the empty one, and the configured ones once
[Token in band](/quest/m2/auth/token-in-band.md) lands), so a peer that never
answers an app-added token cannot suspend enforcement; an app awaits `add`
before publishing what that token unlocks. On a miss, abort the session with
`Error::Unauthorized` and put the path in the close reason (`lite::start`'s
teardown already formats one; log it at error level too, since `Error`
carries no payload).

Gate everything on `Version::Lite06Wip`; older versions never open the stream
and `auth().grant()` stays `None` there. Land the accept side and the
`Unsupported` handling before any build opens the stream, since
`moq-lite-06-wip` is one ALPN with no sub-version.

### JavaScript

Mirror in `js/net/src/lite/`: `stream.ts` gains `Auth: 7`, an `auth.ts` with
the three messages, `connection.ts` opens and answers the streams, and
`Established` in `js/net/src/connection/established.ts` gains `auth` with
`grant: Getter<Grant | undefined>` (a `@moq/signals` signal like `probe`),
`add(token): Promise<Token>`, and `requests()`. The publisher in
`js/net/src/lite/publisher.ts` aborts the connection the same way.

### Docs and tests

Update `doc/concept` where sessions and authorization are described, and the
moq-net guide if it lists stream types. Tests: encode and decode round trips
and the version gate; both sides receive a grant from scoped origins,
including the empty-prefix and empty-list spellings; a publish-only session
grants no subscribe; an out-of-scope announce aborts with `Unauthorized` and
names the path in the reason, while an in-scope one does not; two tokens union
and closing one stream shrinks the union on both sides; an update AUTH_OK
replaces one token's grant without touching the other; an unanswered
app-added token does not suspend the check; a reset AUTH stream reports
`Unsupported`; Rust to JS and JS to Rust interop in the existing
cross-language harness.

On main, additive.

## Related

- [Pattern interest](/quest/m2/path-patterns/interest.md) - moves the prefix
  fields here and in ANNOUNCE_REQUEST to patterns together
