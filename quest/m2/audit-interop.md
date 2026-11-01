# [M] Interop audit

## Goal

Our moq.dev entries in [englishm/moq-interop-runner](https://github.com/englishm/moq-interop-runner)
match what we ship: the client builds on the latest published moq-tokio and
`@moq/net`, the declared draft versions, and the relay endpoints. Every red
cell with a moq.dev side on <https://git.moq.dev/relays/> and on the
runner's report is classified from its logs and either fixed (ours),
reported upstream (theirs), noted as down, or noted as unsupported (a
feature outside the moq-net API).

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
  Smoke's upstream runner job fails only on our clients against our relay,
  so this audit is what catches every other red cell.
- Sync new registry entries. The smoke workflow's upstream runner job runs
  the runner's HEAD, so check that newly registered clients and docker-only
  relays show up in its [report](https://git.moq.dev/interop-runner/),
  and ask upstream for a published image for any it lists as not run. The relays page reads every
  remote relay from the registry; give new relays the per-relay config they
  need (`publish_path`, `WEBSOCKET_KEYS` in `test/smoke/relays.sh`).
- Standing approval to open PRs on englishm/moq-interop-runner that touch only
  the `moq-dev-*` builds and registry entries, and to file reproducer issues
  on other implementations' repos. Every post ends with the AI marker.

Both reports run published packages, so a fix turns green only after it
ships. Verify from `main` instead. For our clients through a third-party
relay, `CLI_BIN=target/debug/moq just relays --only <key> --publishers rust
--subscribers rust` from `test/smoke`. For a third-party client through our
relay, run its published runner image (`RELAY_URL`, `TLS_DISABLE_VERIFY=1`,
`TESTCASE`) on the host network against a `moq-relay` built from `main`.
For the runner's own cases from `main`, build its `moq-dev-rs` or
`moq-dev-js` client against this checkout; both need small ports to
`main`'s API. Read the cell's log before classifying.

### Check first

The m1 run (2026-10-23) read the runner's 2026-10-23 nightly (77 of 139
moq.dev cells red; clients on moq-tokio 0.19.28 and `@moq/net` 0.4.4,
docker relay moq-relay 0.17.7) and the relays page (40 of 103 red). Most
red cells were already fixed on `main` and wait for the cut, so confirm
each turned green:

- The runner's clients bumped to the cut, including the API port the
  [runner client bump](/quest/m2/release/runner-client-bump.md) describes.
  The JS client's `moqt://` dials (m0 fix) then reach every relay serving
  WebTransport on that port; a raw-QUIC-only relay stays unsupported, since
  JS has no raw QUIC. The JS client's red5 WebTransport cells still time out
  connecting, which m0's one-write control messages target.
- `subscribe-namespace-lifecycle`: released JS withdraws a draft-18+
  PUBLISH_NAMESPACE with FIN, which six relays ignore, so it waits for
  NAMESPACE_DONE. `main` resets the request
  ([moq-dev/moq#5483](https://github.com/moq-dev/moq/pull/5483)) and passes
  against Cloudflare draft-18;
  [moq-dev/moq#6518](https://github.com/moq-dev/moq/pull/6518) backports it
  to `release`. Released Rust never sees the NAMESPACE itself from quic-zig
  or moxygen (docker); the client built on
  `main` passes against both, so it gets no backport: the runner client bump
  fixes it.
- Our relay through third-party clients: `rendezvous-timeout` 0x10
  (aiomoqt, imquic, quic-zig, Cloudflare draft-18), the relay stamping a
  Timestamp property on every object
  ([moq-dev/moq#5083](https://github.com/moq-dev/moq/pull/5083)), and moq-go
  refusing a server without RESET_STREAM_AT (moq-quic) are fixed on `main`,
  as are xquic's raw-QUIC connection errors (5 of 6 cases pass). An omitted
  DEFAULT_PUBLISHER_PRIORITY or SUBSCRIBER_PRIORITY now forwards as 128, not 127.
- Through our relay, Cloudflare's draft-18 `data-subgroup-basic` still
  fails on two items reported upstream:
  [cloudflare/moq-rs#281](https://github.com/cloudflare/moq-rs/issues/281)
  (its SUBSCRIBE_OK claims Largest Object (0, 0) before publishing it, then
  refuses the joining FETCH) and
  [cloudflare/moq-rs#282](https://github.com/cloudflare/moq-rs/issues/282)
  (it fails our relay's END_OF_TRACK).

Still red from `main`, ours:

- moq-go's webtransport-go speaks WebTransport draft 15: it requires
  SETTINGS_WT_ENABLED (0x2c7cf000) and sends `:protocol webtransport-h3`.
  `web-transport-proto` 0.6.2 (moq-dev/web-transport) sends neither and
  refuses the new token, so every moq-go cell against our relay fails.
- moq-dev-js `data-subgroup-basic` against libquicr never sees the track
  end, while moq-dev-rs passes. Not yet traced.
- Every `js-web` publisher cell behind a joining FETCH (imquic, MOQtail
  draft-22, aiomoqt, red5) was refused its FETCH; JS now serves cached and
  draft-18 joining FETCH, so rerun them.

Theirs, reported or waiting on others:

- Reported before and still open:
  [moqtail/moqtail#450](https://github.com/moqtail/moqtail/issues/450),
  [openmoq/moqx#852](https://github.com/openmoq/moqx/issues/852),
  [gmarzot/aiomoqt#123](https://github.com/gmarzot/aiomoqt/issues/123),
  [yatagai-mm/Moqtopus#2](https://github.com/yatagai-mm/Moqtopus/issues/2),
  [facebookexperimental/moxygen#252](https://github.com/facebookexperimental/moxygen/issues/252),
  and [facebookexperimental/moxygen#225](https://github.com/facebookexperimental/moxygen/issues/225)
  (filter type 250, which also fails moxygen's and moqx's
  `announce-subscribe` against our relay).
- [meetecho/imquic#42](https://github.com/meetecho/imquic/issues/42) is
  fixed upstream; imquic's cells against `cdn.moq.dev` wait for the runner
  to rebuild its image.
  [meetecho/imquic#43](https://github.com/meetecho/imquic/issues/43)
  (drops a data stream whose header spans reads) is owned by
  [JS subgroup data through imquic](/quest/m1/interop-coverage/js-imquic-objects.md).
  [Eyevinn/moqlivemock#157](https://github.com/Eyevinn/moqlivemock/issues/157)
  is fixed and green.
- Drafted in this run, posted only if the maintainer approved: MOQtail draft-18 never forwards
  NAMESPACE for a later matching PUBLISH_NAMESPACE; moq-go dials only the
  IPv6 address of `cdn.moq.dev` and needs an explicit port in an `https://`
  URL; and the runner lists moqxr as not run for lack of a published image.
  Search before filing again.
- To file: cloudflare/moq-rs `SubscribeRecv::error` returns before setting
  `closed` when `TrackWriter::close` fails because the track has no reader,
  so a REQUEST_ERROR reaches the caller as `done` instead of not-found.
  Smoke's Cloudflare probe holds its reader to avoid it
  ([moq-dev/moq#7073](https://github.com/moq-dev/moq/pull/7073)).
- Not reported, lower value: red5 subscribes the published prefix instead
  of the requested tuple namespace (no public repo); moqlivemock's relay
  refuses a SUBSCRIBE under an announced prefix; libquicr answers
  SUBSCRIBE_OK for an unpublished namespace; xquic sends `moq-test/interop`
  as one tuple field, and its WebTransport never reaches our relay.

Unsupported or down, not worth rechecking unless something changes:
PUBLISH and SUBSCRIBE_TRACKS cases (`publish-track-*`, `namespace-discovery`,
Cloudflare's PUBLISH cases), relays without namespace discovery, which
moq-net needs before a SUBSCRIBE (xquic-draft-18), peers speaking only
moq-transport drafts 14 to 17 (Cloudflare's draft-14 and draft-16 relays),
quiche-moq (connect idle timeout), xquic's docker relay (binds loopback),
and moqtopus and xquic-draft-18 over WebTransport (raw QUIC only).

## Required

- [Release cut](/quest/m2/release/cut.md) - waits for the release in flight to ship

From the 2026-10-25 run: the runner's moq-dev-js `announce-subscribe` case times
out against imquic on both 0.4.4 and the [moq-dev/moq#6769](https://github.com/moq-dev/moq/pull/6769)
backport, so it predates that fix. Classify it.

## Related

- [Runner client bump](/quest/m2/release/runner-client-bump.md) - the runner's Rust client moves to the cut and its API
- [Runner teardown](/quest/m3/interop-runner-approval.md) - the runner's moq.dev clients move to the withdrawing `close()` once it ships
- [Interop coverage](/quest/m1/interop-coverage/README.md) - the runner cases whose red cells this audit classifies
