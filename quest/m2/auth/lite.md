# [L] Lite stream

## Goal

Both sides of a moq-lite-06 session learn what they may publish and subscribe
to. Right after SETUP each side opens an AUTH stream, sends an empty token, and
receives its grant; `Session::auth()` exposes that grant and lets the app send
a new token on the same stream. A publisher whose origin announces a broadcast
outside its grant aborts the session with `Unauthorized` naming the path
instead of waiting for a solicitation that never comes. Rust and JavaScript
both speak it, and the lite draft specifies it.

## Plan

### Wire

Add stream type `0x7` AUTH, creator either, to the bidirectional stream table
in `drafts/draft-lcurley-moq-lite.md` and a section beside Probe modeled on
it. Messages, encoded like PROBE with the `(i)`, `(b)`, `(s)` notation the
draft uses:

- `AUTH { Sequence (i), Token (b) }`, written only by the opener. Sequence
  starts at 1 and increments per AUTH. An empty token means the credential the
  connection already presented, or nothing.
- `AUTH_OK { Sequence (i), Publish Count (i), Publish Prefix (s)...,
  Subscribe Count (i), Subscribe Prefix (s)..., Expires (i) }`, written only
  by the acceptor. The prefixes use the encoding and rules ANNOUNCE_REQUEST's
  prefix uses: a list holding the empty prefix grants everything, an empty
  list grants nothing. Expires is milliseconds until the grant lapses, zero
  for never.
- `AUTH_ERROR { Sequence (i), Code (i), Reason (s) }`, written only by the
  acceptor.

Every AUTH is answered by exactly one AUTH_OK or AUTH_ERROR echoing its
Sequence. The acceptor MAY also write an AUTH_OK with Sequence 0 unprompted
when the grant changes, so an update and a reply are never confused however
they interleave; the latest AUTH_OK is always the grant in effect. An
AUTH_ERROR leaves the previous grant in effect. A reply whose Sequence was
never sent, or was already answered, is a protocol violation. The stream
lives as long as the session; resetting it is not a refusal, and a peer that
resets an AUTH stream it does not understand (the existing unknown-stream rule
at the STREAM_TYPE section) leaves the opener with no grant, which the client
reports as `Unsupported` rather than treating as a refusal. Record it in the
lite-06 changelog. Run `just drafts check`.

### Model

New module `rs/moq-net/src/auth.rs`, public as `moq_net::auth`:

- `auth::Grant { publish: PathPrefixes, subscribe: PathPrefixes, expires:
  Option<Instant> }`, a plain value in the presenter's root.
- `auth::Handle`, cloneable, returned by `Session::auth()`. `grant()` is a
  watchable current value (`None` until the first AUTH_OK, the `kio` shared
  cell the session already uses for the peer SETUP is the pattern), and
  `refresh(token) -> Result<Grant, Error>` writes an AUTH and resolves with the
  reply carrying its Sequence. `requests()` yields `auth::Request { token,
  accept(Grant), reject(code, reason) }` for the acceptor side; a request left
  unanswered is a bug, so dropping one rejects with `Unauthorized`.
- The default acceptor needs no app code, and whether it is in charge is
  decided once, before the driver starts, never per message: the server-side
  `moq_net::Request` builder exposes `auth()` before `.ok()`, and a caller
  that takes `requests()` there owns every AUTH the session receives, the
  initial empty one included. With no such consumer, the driver answers every
  empty token from the session's own origin handles, publish
  from the subscriber half's `origin::Producer::allowed()` and subscribe from
  the publisher half's `origin::Consumer::allowed()`, with no expiry. A missing
  half grants nothing. That is what makes the cluster case free: both relays
  present mTLS, both learn the unrestricted grant. A non-empty token with no
  consumer is rejected `Unsupported`; the relay is the consumer in
  [Relay refresh](/quest/m2/auth/relay-refresh.md).

`Session` is protocol-agnostic (`rs/moq-net/src/session.rs` holds a version,
bandwidth handles, and a boxed driver), so `lite::start` builds the handle and
hands it to `Session::new`; `ietf::start` hands one whose `grant()` stays
`None` and whose `refresh` fails `Unsupported` until
[moq-transport](/quest/m2/auth/moq-transport.md) fills it in.

### Session

Accepted bidi streams are dispatched by the publisher half alone
(`Publisher::handle` in `rs/moq-net/src/lite/publisher.rs`), which holds only
the publish-side `origin::Consumer`; the grant needs both halves' `allowed()`,
so the handle is built in `lite::start` with both handles and shared into the
publisher and subscriber, the way the peer-setup slot is. Opening: the
subscriber half opens the AUTH stream where it opens Probe, after the peer
SETUP resolves, and writes the empty AUTH. Accepting: a new `recv_auth` beside
`recv_probe` answers each AUTH, on error aborting only that stream.

Fail loud: the publisher half already scopes its origin per solicited prefix;
add a check that every path its origin announces is covered by
`grant.publish`, run when the grant arrives and when a broadcast is announced,
and only while no AUTH is outstanding, so the initial empty AUTH and a
widening refresh cannot race a publish into a false refusal. On a miss, abort
the session with `Error::Unauthorized` and put the path in the close reason
(`lite::start`'s teardown already formats one; log it at error level too,
since `Error` carries no payload).

Gate everything on `Version::Lite06Wip`; older versions never open the stream
and `auth().grant()` stays `None` there. Land the accept side and the
`Unsupported` handling before any build opens the stream, since
`moq-lite-06-wip` is one ALPN with no sub-version.

### JavaScript

Mirror in `js/net/src/lite/`: `stream.ts` gains `Auth: 7`, an `auth.ts` with
the three messages, `connection.ts` opens and answers the stream, and
`Established` in `js/net/src/connection/established.ts` gains `auth` with
`grant: Getter<Grant | undefined>` (a `@moq/signals` signal like `probe`),
`refresh(token): Promise<Grant>`, and `requests()`. The publisher in
`js/net/src/lite/publisher.ts` aborts the connection the same way.

### Docs and tests

Update `doc/concept` where sessions and authorization are described, and the
moq-net guide if it lists stream types. Tests: encode and decode round trips
and the version gate; both sides receive a grant from scoped origins,
including the empty-prefix and empty-list spellings; a publish-only session
grants no subscribe; an out-of-scope announce aborts with `Unauthorized` and
names the path in the reason, while an in-scope one does not; a refresh
resolves with the reply echoing its Sequence even when an unprompted AUTH_OK
arrives first, and the unprompted one still replaces the grant; a reset AUTH
stream reports `Unsupported`; Rust to JS and JS to Rust interop in
the existing cross-language harness.

On main, additive.

## Related

- [Pattern interest](/quest/m2/path-patterns/interest.md) - moves the prefix
  fields here and in ANNOUNCE_REQUEST to patterns together
