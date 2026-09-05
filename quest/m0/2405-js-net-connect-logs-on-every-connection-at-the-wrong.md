# [S] js/net: connect logs print the JWT in the relay URL

## Goal

No `@moq/net` log line ever contains a credential. A relay URL is logged as
origin plus path, never its query, and the connect diagnostics are silent in a
production build.

## Plan

`js/net/src/connection/connect.ts` passes `url.toString()` to three log calls:
the WebSocket fallback warning, the WebTransport debug line, and the negotiated
ALPN debug line. A URL carrying `?jwt=` therefore reaches the console and any
attached log collector on every connect. The fingerprint fetch is the one site
that already scrubs (`search = ""` before logging), and there is no shared
helper.

- Add one redacting formatter in `js/net` that renders `origin + pathname` and
  use it at every URL log site, including the fingerprint one, so the rule
  lives in one place rather than at each call.
- Gate the connect diagnostics on the `DEV` check `js/signals` already uses
  (`import.meta.env.MODE !== "production"`), so a consumer's production build
  prints nothing. The WebSocket warning is the one line worth keeping visible
  in dev, since it means the transport race lost.
- Test: connect against a URL carrying `?jwt=secret` with the console captured
  and assert no logged string contains the token; and under a production
  `MODE`, assert no connect diagnostic is emitted at all, so a redacted URL
  logged in production still fails.

The moq-native and moq-rtmp half of the same finding is already fixed, and its
PR carries the issue-closing keyword.
