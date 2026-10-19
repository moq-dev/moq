# [S] A request right behind its token is admitted

## Goal

On moq-lite, a request that only a just-presented token admits is never
refused because it reached the acceptor before that token's AUTH settled, and
a renewal never drops a live request the new grant still covers. Lite
requests stay judged by the union of session-lifetime auths alone: no Auth ID
on AUTH, no `Auth` field on requests, no extra bytes on any request, and no
timer.

## Plan

- ✅ Decided 2026-10-19 (maintainer, closing
  [#5927](https://github.com/moq-dev/moq/pull/5927)): drop the named version.
  "I don't want per-request auth and I don't reaaaallly want to waste a byte
  on every request." Lite AUTH carries no ID and lite requests no `Auth`
  field. `main` never carried either (checked 2026-10-19 in
  `drafts/draft-lcurley-moq-lite.md`, `rs/moq-net` and `js/net`), so nothing
  needs removing.
- ✅ Decided 2026-10-19 (maintainer): the first token rides the CONNECT
  request, the URL query today and an auth header once browsers allow one, so
  a session's first requests wait on no AUTH. Further tokens join the union
  through AUTH streams. [Token in band](/quest/m1/auth/token-in-band.md)
  follows this.
- ✅ Decided 2026-10-19 (maintainer): moq-transport's per-request
  AUTHORIZATION TOKEN stays acceptor-side as
  [#5148](https://github.com/moq-dev/moq/pull/5148) modeled it and never
  becomes a lite wire feature. `draft-lcurley-moq-auth` gains no request
  parameter naming an AUTH, and token aliases stay unsupported (cache size 0).
- Where `main` stands (2026-10-19). A lite acceptor judges SUBSCRIBE and
  FETCH against its scope when it decodes them, and a grant joins the scope
  only once the acceptor accepts its token. The presenter's own gate refuses
  a request its known union does not cover before sending it, and
  `auth::Handle::add` (Rust) and `add` (js/net) resolve only after AUTH_OK, so
  an app that awaits `add()` never races. Announcements already heal: a
  broadcast withheld for falling outside the scope comes back when the scope
  widens. What is left:
  - the draft does not say any of this;
  - nothing pins that an acceptor widens its scope no later than it sends
    AUTH_OK, which the presenter relies on;
  - tokens the library presents without the app awaiting them race the app's
    requests on the same session: moq-tokio's reconnect replay in
    `connection::auth` today, and later the session-start tokens of
    [Auth outlives its session](/quest/m1/auth/issuer.md) and
    [Token in band](/quest/m1/auth/token-in-band.md). A multi-token client
    is the one that can hit it.
- Open (maintainer): how a request right behind its token avoids refusal.
  - (a) The acceptor waits. A request it would refuse while any AUTH from that
    peer is unanswered is held until those settle, then judged against the
    union. Zero bytes, zero messages. It does not close the race on lite: the
    AUTH and the request ride different streams, so a lost AUTH packet lets
    the request arrive first with nothing pending, and WebTransport hides the
    stream ordering that could tell the acceptor an earlier stream exists. The
    presenter must also stop refusing locally and send optimistically. Abuse:
    a peer presenting AUTH after AUTH keeps "something pending" true forever,
    so the wait must be on a snapshot of the AUTHs pending when the request
    arrived. A held request keeps only the peer's own slot, and the
    acceptor's verifier, not the peer, decides how long it waits.
  - (b) AUTH_WAIT. A presenter sends it ahead of a request, on the request's
    own stream, only while one of its AUTHs is unanswered. It carries how many
    Auth Streams the sender has opened so far, and the acceptor holds the
    request until it has received that many and each is answered or
    withdrawn, then judges against the union. Exact, and it saves the one
    round trip (c) spends, with bytes only on those requests. Abuse: a count
    the peer never reaches holds only its own request, and a count beyond the
    bidirectional-stream credit the acceptor ever granted is a
    PROTOCOL_VIOLATION. Costs a new lite-07-wip message, and adding it after
    the cut costs lite-08.
  - (c) The presenter waits (recommended). A request its known union does not
    cover waits, instead of failing locally, while any non-empty token this
    side presented is unanswered, then goes out or fails against the settled
    union. An unknown union covers nothing for this rule, but the empty token
    never holds a request, since the acceptor judges the CONNECT credential
    before the session starts. It costs one round trip, and only for the first
    request relying on a new token, which `add()` already spends; a
    single-credential session and a renewal spend nothing. No wire change,
    no acceptor state, and the rule lives in the shared gate, so the
    moq-transport AUTH extension gets it too. Abuse: an acceptor that never
    answers holds only our own requests, which it could stall anyway by never
    answering them.
- Under (c): spec in lite-07-wip that the acceptor judges a request against
  the union when it reads it, that its scope includes a grant no later than
  it sends that AUTH_OK, and that a presenter does not send a request only an
  unanswered token admits. Hold the publish-outside-the-union check to the
  same rule, so it waits on every unanswered token rather than only the setup
  ones. Mirror in `js/net`.
- Renewal under the union: present the new token, wait for its AUTH_OK
  (`add()` already does), then drop the old one. The union never shrinks
  under a live request the new grant also covers, and a narrower renewal ends
  only what it no longer covers.
- Not carried from #5927: FIN-linger withdrawal, since nothing names the old
  token and dropping it after the new AUTH_OK races nothing; per-token
  wakeups and the `named` bench, since no request watches one token. A change
  still recomputes the union, as today. Extend `rs/moq-net/benches/auth.rs`
  only if the wait adds per-request work on an AUTH_OK.
- Tests that fail without the fix, in Rust and JS: a direct request made
  right after a reconnect replays a token is admitted; an acceptor's AUTH_OK
  never precedes its scope; a live subscription survives a renewal.
- Docs: renewal and the wait in `doc/concept/moq-lite.md`'s Authorization
  section.

Public API: none. Wire: lite-07-wip draft text only under (c); a new
lite-07-wip message under (b), which makes this [M].

## Related

- [#5927](https://github.com/moq-dev/moq/pull/5927) - the closed named-auth
  version this replaces
- [Live requests follow their auth](/quest/m1/auth/request-update.md) -
  moq-transport request renewal; lite renews through the union
