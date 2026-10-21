# [M] Interop audit

## Goal

Our moq.dev entries in [englishm/moq-interop-runner](https://github.com/englishm/moq-interop-runner)
match what we ship: the client builds on the latest published moq-tokio and
`@moq/net`, the declared draft versions, and the relay endpoints. Every red
cell with a moq.dev side on <https://git.moq.dev/relays/> and on the
runner's report is classified from its logs and either fixed (ours),
reported upstream (theirs), or noted as down.

## Plan

Recurs once per major release (decided 2026-10-21). The PR that finishes
this run also adds the next one with the same file name at the end of the next
milestone's `Required` (m0 to m1, m1 to m2). The successor keeps the Goal
and this paragraph; the rest of its Plan is rewritten with what this run
learned: baselines, results, and what to check first. A successor placed in m2
also lists the [release cut](/quest/m2/release/cut.md) under `Required`, so it
waits for the release in flight to ship. Only `/quest-delete` ends the
recurrence. Fixes land in the run's PR, one commit each, rather than as follow-ups.
Only a change that needs a maintainer decision (public API, scope) or is
independently landable and too large to review alongside the rest becomes a
follow-up through `/quest-plan`.

Decided with the maintainer on 2026-10-21:

- Scope: the relays page (our published Rust, browser, and Bun clients
  through every registered public relay), plus every cell of the runner's
  report with a moq.dev side: our clients, our docker relay, or
  `cdn.moq.dev`, including third-party clients failing against our relay.
- Sync new registry entries. Once the [runner nightly](/quest/m1/interop-runner-nightly.md)
  lands, it runs the runner's HEAD, so check that newly registered clients
  and docker-only relays show up in its report. On the relays page, drop an `EXTRA_ENDPOINTS` entry
  in `test/smoke/relays.sh` once upstream registers it, and give new relays
  the per-relay config they need (`publish_path`, `WEBSOCKET_KEYS`).
- Standing approval to open PRs on englishm/moq-interop-runner that touch only
  the `moq-dev-*` builds and registry entries, and to file reproducer issues
  on other implementations' repos. Every post ends with the AI marker.

Both reports run published packages, so a fix turns green only after it
ships. Verify a fix from `main` instead: `test/smoke/smoke.sh` with
`RELAY_BIN` and `MOQ_BIN`, or `test/interop`, pointed at the failing relay.
Classify from the cell's logs, not its summary: `failure_reason` in
`test/smoke/smoke.sh` takes the first error-looking line, which is often
teardown noise after the 30 s timeout.

### Check first

From the relays run of 2026-10-21 ([run](https://github.com/moq-dev/moq/actions/runs/39608934024),
moq-cli 0.14.7, `@moq/net` 0.4.4), reproduced live against each relay.
Ours, to fix in this run:

- The Rust client offers `moq-00` over WebTransport
  (`rs/moq-net/src/version.rs`), so moxygen picks draft-14, where we send an
  empty-prefix SUBSCRIBE_NAMESPACE that is illegal before draft-16 (moxygen
  answers 0x10 "empty"; Cloudflare draft-14 never answers). `main` skips the
  empty prefix (#5018) and then announces nothing. Scope the CLI
  subscriber's origin to `--broadcast` as the RTC and HLS importers do, and
  stop offering `moq-00` over WebTransport.
- WT_SESSION_GONE (0x170d7b68) resets surface as `invalid RESET_STREAM`
  transport errors (`rs/moq-tokio/src/quic/web/recv.rs`). Per
  draft-ietf-webtrans-http3 §4.4 an unmappable code is still a reset; map
  SESSION_GONE to the session's close reason. Log unknown capsules at
  DEBUG, not WARN.
- js/net writes each control message as id, length, and body in separate
  writes (`js/net/src/ietf/message.ts`, `connection/handshake.ts`). Encode
  once and write once, as Rust does. red5 only parses a SETUP that arrives
  in one frame, and JS waits for its SETUP with no deadline, so the
  session silently stalls.
- The JS subscriber joins video unfiltered before draft-20
  (`js/net/src/ietf/subscribe.ts`) and then stops every headless group
  stream. Asking for Next Group Start avoids requesting what it discards.
- The JS publisher refuses every FETCH, so a relay that fills with an
  upstream FETCH still fails against it. Owned by
  [JavaScript FETCH](/quest/m1/js-fetch.md).
- `failure_reason` should report the last stage the subscriber reached
  (connected, announced, SUBSCRIBE_OK, no objects) instead of the first
  error line.
- Our registry entries are stale: moq-dev-rs declares drafts 14-20 and
  moq-dev-js 14-18, though both negotiate draft-22.
- Runner report of 2026-10-19: moq-relay fails `data-subgroup-basic`
  (owned by [runner cases](/quest/m1/interop-coverage/runner-cases.md)) and
  answers `rendezvous-timeout` with 0x10 instead of TIMEOUT. Cloudflare
  moq-rs fails ten cases because moq-relay declines PUBLISH, which is out of
  scope (not in the moq-net API).
- Unchased: a late-joining Rust subscriber on moqx got an empty catalog
  group and its joining FETCH never yielded the object.

Theirs, to report upstream:

- imquic does not replay existing namespaces to a later
  SUBSCRIBE_NAMESPACE, so a subscriber that arrives after the publisher sees
  nothing on either transport; the WebTransport/QUIC split is that race.
- MOQtail draft-18 never forwards SUBSCRIBE and never answers it.
- Nokia and MOQtail draft-22 narrow a downstream join to Next Object with no
  FILL or upstream FETCH (draft-22 §3.2.3, §7.4), so a catalog written once
  never arrives. MOQtail resets with INTERNAL_ERROR (0).
- aiomoqt, moxygen, and moqx end the whole subscription when a subscriber
  sends STOP_SENDING on one subgroup stream (draft-22 §11.3.2 ends only that
  subgroup).
- red5 parses SETUP only when it arrives in one stream frame (RFC 9000 §2).

Down or misconfigured: quiche-moq (connect idle timeout), Cloudflare
draft-16 (closes with "scope resolution failed"; check the URL in
`EXTRA_ENDPOINTS`).

## Related

- [Runner nightly](/quest/m1/interop-runner-nightly.md) - runs every registered client and docker relay against us
- [Runner teardown](/quest/m3/interop-runner-approval.md) - the runner's moq.dev clients move to the withdrawing `close()` once it ships
- [Moxygen minimal interop](/quest/m1/smoke-moxygen.md) - Moxygen's filter type, waiting upstream
