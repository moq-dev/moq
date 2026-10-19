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
- ✅ Decided 2026-10-19 (maintainer, #5972): the first token rides SETUP, a
  lite-07-wip SETUP parameter and moq-transport's existing AUTHORIZATION
  TOKEN option, with the URL query as the fallback. "We need to block on
  SETUP anyway": the acceptor already reads the client's SETUP before serving
  the session (`Server::accept_request_lite`), and a lite client sends
  requests without waiting on the server's, so it costs no round trip and a
  session's first requests wait on no AUTH. Further tokens join the union
  through AUTH streams. [Token in band](/quest/m1/auth/token-in-band.md)
  carries the parameter. This replaces the earlier "always in the CONNECT
  request" decision and drops its auth-header plan.
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
    `connection::auth` today, and later the extra session-start tokens of
    [Auth outlives its session](/quest/m1/auth/issuer.md) and
    [Token in band](/quest/m1/auth/token-in-band.md). The SETUP token never
    races, so only a client with more than one token can hit it.
- ✅ Decided 2026-10-19 (maintainer, #5972): **the presenter waits**, with a
  log line so we see how often it triggers. "I want to make sure we're not
  adding dumb RTTs." A request its known union does not cover waits, instead
  of failing locally, while any non-empty token this side presented is
  unanswered, then goes out or fails against the settled union. An unknown
  union covers nothing for this rule, but the empty token never holds a
  request, since the acceptor judges the connection credential (SETUP or URL)
  before it serves the session. It costs one round trip, only for the first request relying on a new
  token, which `add()` already spends; a single-credential session and a
  renewal spend nothing. No wire change and no acceptor state. The rule lives
  in the shared gate, so the moq-transport AUTH extension gets it too. Abuse:
  an acceptor that never answers holds only our own requests, which it could
  stall anyway by never answering them. No timer.
- Rejected 2026-10-19 (#5972): the acceptor waiting on that peer's unanswered
  AUTHs, because AUTH and the request ride different streams, so a lost AUTH
  packet still lets the request arrive first with nothing pending; and an
  AUTH_WAIT prefix on the request stream, which is exact but a new message to
  save a round trip nobody has measured. Revisit AUTH_WAIT only if the log
  below shows the wait firing in practice.
- ✅ Decided 2026-10-19 (maintainer, #5972): observability is the log line
  only, not a `stats::Session` counter. Every request that waits on an
  unanswered token logs once (path and how long it waited), in Rust and
  `js/net`.
- The wait only sees tokens already presented, so a replayed token must be
  registered on the new session before the app can use it. Today
  `Shared::connected` in moq-tokio publishes the new session before each
  per-token replay task reaches `add()`, so a woken app can send a request
  while nothing is pending. Present every token the connection holds on the
  new session's handle (queued, not awaited) before publishing the session;
  [Auth outlives its session](/quest/m1/auth/issuer.md) does this by
  construction once the handle spans sessions. js/net mirrors it if its
  reconnect replays tokens the same way.
- Spec in lite-07-wip that the acceptor judges a request against the union
  when it reads it, that its scope includes a grant no later than it sends
  that AUTH_OK, and that a presenter does not send a request only an
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
  the moment a reconnect publishes its session, relying on a replayed token,
  is admitted; an acceptor's AUTH_OK
  never precedes its scope; a live subscription survives a renewal.
- Docs: renewal and the wait in `doc/concept/moq-lite.md`'s Authorization
  section.

Public API: none. Wire: lite-07-wip draft text only, no new bytes.

## Related

- [#5927](https://github.com/moq-dev/moq/pull/5927) - the closed named-auth
  version this replaces
- [Live requests follow their auth](/quest/m1/auth/request-update.md) -
  moq-transport request renewal; lite renews through the union
