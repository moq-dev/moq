# [S] Transport preference has a justified lifetime

## Goal

Connection racing retains useful fallback knowledge without accumulating
unneeded URL history for the whole page lifetime.

## Plan

[Connection racing](/js/net/src/connection/connect.ts) stores each WebSocket
winning full URL in module-level `websocketWon`, with no deletion. A later
WebTransport win does not clear it. Comments call this the last race, while
current behavior remembers whether WebSocket ever won.
[moq-tokio](/rs/moq-tokio/src/websocket.rs) instead keys the host and port the
fallback dials and forgets it when QUIC wins.

The browser benchmark's `racing` scenario
([bench README](/bench/README.md#browser)) measured the baseline in Chromium
156 against a local relay, with an A/A control at 0% retained-string spread:

- Retention is one string per distinct URL whose dial WebSocket won, across
  1/16/128 URLs and 1/4/16 reconnects; disabled fallback and WebTransport wins
  retain none. Each is a 60-byte V8 string plus its characters
  (142 to 2,126 per URL), so 128 URLs keep about 50 KB. Memory is a no-win.
- The preference saves its head start: a repeat WebSocket dial takes 11 ms
  instead of 503 ms.
- A query rotated on every reconnect, as a `jwt` refresh is, adds an entry
  per dial and never hits it: every reconnect pays 503 ms. The retained keys
  hold the token for the page's lifetime.
- After one WebSocket win, dials with WebTransport open again still landed
  on WebSocket 20% (second dial) and 90% (third) of the time on loopback,
  where the development certificate fetch slows WebTransport.

Open for the maintainer: key on the full URL, the URL without its `jwt`, or
the fallback's host and port as moq-tokio does; and whether a WebTransport win
clears the entry. Then apply them in `connect.ts`, fix the comment, unit-test
the rule, and compare `just bench-browser --scenarios racing <base>`.
Do not silently collapse endpoint-specific behavior into origin-only keys or
pick an arbitrary TTL. No API or wire change is expected.
