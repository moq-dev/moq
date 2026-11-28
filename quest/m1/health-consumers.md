# [M] Browser reporters feed client health

## Goal

The client health grade from
[moq-dev/moq#6654](https://github.com/moq-dev/moq/pull/6654) grades a browser
publisher's loss and headroom, and its limits are re-measured against the
numbers the browser reporters publish.

## Plan

Done in the quest's PR: `transport.sentBytes` (the connection's cumulative
bytes sent) in the `stats` and `echo` snapshots, loss and headroom graded over
it so they hold for a connection carrying several broadcasts, `moq export
health` as the first consumer, and `moq import --report` reporting it. `moq-dev/studio` m0 (`quest/m0/pin-moq-tip.md`) and
`moq-dev/hang.live` m2 (`quest/m2/network-xray.md`, reading `Hang.Health`) read
the grade in their own repositories once they move to a `main` with it, and
[Preflight](/quest/m3/stats/preflight.md) will too.

- `<moq-publish stats>` and `<moq-watch echo>` report only the PROBE round-trip
  time (`js/publish/src/broadcast.ts` `#stats`, `js/watch/src/echo.ts`), so a
  browser publisher grades on RTT alone. Report `sentBytes`, `uptime`, `lostBytes`,
  `lostPackets`, and `rate` from `connection.stats()` (`js/net`
  `Connection.Stats`, `bytesSent`, `bytesLost`, `packetsLost`,
  `estimatedSendRate`). It is an async pull, so the reporters need a polled
  stats signal on `Connection` beside `probe` instead of the synchronous
  read they use now; W3C's `bytesSent` excludes retransmissions and QUIC
  overhead, so the browser's loss reads higher than native for the same link.
- Re-measure the thresholds in `rs/hang/src/health.rs` against what the browser
  reporters then publish, with `moq-shaper` impairing the link by bufferbloat
  (a finite queue), not random loss, in a browser driven by `agent-browser`.
  The recorded limits divided by media bytes on native QUIC; native `sentBytes`
  adds overhead and retransmissions, which nudges headroom toward worse and
  loss toward better.
- The demo dashboard (`demo/web`) does not show the grade yet.
