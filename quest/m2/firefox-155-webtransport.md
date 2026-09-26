# [XS] Firefox 155 negotiates the version by WebTransport subprotocol

## Goal

Firefox 155 reports the negotiated `protocol`, so a Firefox watcher lands on
the version the relay picks from `WT-Available-Protocols` (lite-06 today)
instead of the draft-14 SETUP fallback it used before, as Chrome 143+ does.
js/net's comments and types stop claiming native WebTransport lacks
`protocol`.

## Plan

- By hand against the in-tree relay, confirm Firefox 155 reports a non-empty
  `protocol` and negotiates lite-06, and Firefox 153/154 still reach the
  draft-14 SETUP path. Playwright Firefox has no WebTransport, so this cannot
  run in CI (see [Media QA on other engines](/quest/m2/browser-media-qa-engines.md)).
- Delete the stale comment in `negotiate` (`js/net/src/connection/connect.ts`)
  and the `@ts-expect-error` on `transport.protocol` in
  `js/net/src/connection/accept.ts`, typing it the way `connect.ts` does if the
  DOM lib still lacks the property. The empty-string and `undefined` fallback
  stays for Firefox 153/154 and draft-14 peers.
- Fix any Firefox line in `doc/lib/js/index.md` or `doc/concept/transport.md`
  this makes stale.

Firefox 155 shipped five WebTransport features; decided 2026-09-26:

- **Subprotocols** are the only one MoQ adopts in the browser, and js/net and
  every Rust backend already speak them; this quest is the Firefox check.
- **Send groups** are native only. MoQ wants strict priority between
  subscriptions (audio over video), and browser send groups are flat and
  byte-fair, so js/net keeps the default group and its packed `sendOrder`.
  moq-noq gains send groups in the
  [scheduler](/quest/m1/quic/scheduler.md), and relays use one per broadcast on
  cluster links in [Scope track priority](/quest/m1/track-priority-scope.md).
  Pooling several publishers on one browser connection might want them later,
  but sessions sharing the same content make that murky.
- **`datagrams.createWritable()`** is already feature-detected in js/net and
  web-transport-wasm; nothing to do.
- **`draining`** is not used. Drain stays at the MoQ layer: GOAWAY carries a
  redirect URI and a timeout and works over qmux and WebSocket, while
  `WT_DRAIN_SESSION` is advisory and carries neither (see
  [drain](/quest/m1/drain/README.md)).
- **`exportKeyingMaterial()`** has no consumer: the exporter is per hop, so it
  cannot key e2ee, which is end to end. Binding auth tokens to the TLS session
  is the plausible future use; moq-noq already exposes the exporter, but
  web-transport-moq does not.
