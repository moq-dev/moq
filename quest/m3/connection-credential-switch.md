# [M] A JS connection switches sessions without a gap when its credential changes

## Goal

When a `@moq/net` connection's credential changes and the negotiated version
cannot carry it in band, the client establishes the session on the new
credential before it closes the old one, so a room participant's broadcasts
stay announced and watched throughout.

## Plan

Decided 2026-11-26 (maintainer, /quest-plan of moq-dev/hang.live's
follow-ups): plan this upstream rather than work around it in Hang (rejected:
leave it until lite-07 in-band renewal is the default).

Found by [moq-dev/hang.live#53](https://github.com/moq-dev/hang.live/pull/53),
which makes room tokens expire after 2 hours and renews them through the
connection's `tokens` signal. On lite-06 the relay ends the session when the
token it dialed with expires, and the client redials on the renewed one. With
the moq pinned in hang.live (`28d8eb84d6`), a participant vanished from the
other tabs for about 0.6s on each renewal. In-band renewal works only on
`moq-lite-07-wip`, which the client takes only when asked and that pin's relay
refuses.

- Reproduce first as a `js/net` connection test: a credential replaced on a
  lite-06 session leaves a window with no established session.
- Recommended fix: treat a credential the live session cannot carry like a
  GOAWAY handover. Dial a successor with the new credential and drain the old
  session only once the successor is established, reusing the predecessor
  drain in `js/net/src/connection/` (see `migrate.test.ts` and
  `reload.test.ts`). Replace the credential before the old token expires, so
  the relay never closes the old session first.
- Keep in-band renewal as the path whenever the version carries it; this is
  the fallback for versions that cannot.

Related: [room participants survive your own connection
blip](/quest/m3/room-keep-through-blip.md) keeps remotes on screen through a
blip, but not your own broadcast as others see it.
[Rust tokens carry the credential in the URL](/quest/m4/tokens-url-auth.md) is
the Rust side of replacing a credential on redial.

Public API: none expected. Wire: none.
