# [S] Drop the client's hold before the server's SETUP

## Goal

A `@moq/net` lite client pipelines its streams again: it no longer holds
everything but AUTH and SETUP until the server's SETUP, so a session's
announcements and subscriptions start one round trip sooner.

## Plan

The hold (`Lines.hold` in `js/net/src/open.ts`, installed by the lite
`Connection` for lite-05+ clients, with the `handshake` open option it exempts)
works around a Chrome renderer crash: a server's `CLOSE_WEBTRANSPORT_SESSION`
capsule, as a refusal sends, arriving while the page has a bidirectional stream
with a read pending and a unidirectional stream with a write in flight, kills
the tab with a null-pointer read. The reproduction, a 100-line aioquic server
and a 30-line page with no MoQ, is attached to the Chromium issue below.

Once a shipping Chrome carries the fix, delete the hold, the `handshake`
option, and the tests asserting the hold (`a hold keeps every open but the
handshake's out of the line` in `open.test.ts`, `opens only its handshake` in
`lite/connection.test.ts`). Keep the mock transport's refusal of creates after
close, which matches browsers. Verify with the reproduction against that Chrome
before deleting.

Wait for the fix to reach Chrome stable, not only Canary: the hold protects
every user still on an affected release.

## Waiting

- Chromium: fixes [issue 589817008](https://issues.chromium.org/issues/589817008)
  and the fix reaches Chrome stable. Check the issue's status and its fixed-in
  milestone, then run the reproduction on that stable release.
