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
ships. Verify a fix from `main` instead: `MOQ_BIN=target/debug/moq just
relays --only <key>` from `test/smoke`, or `test/interop`, pointed at the
failing relay. The relays page's reason column now leads with the last stage
the subscriber reached (connected, SUBSCRIBE sent, SUBSCRIBE_OK, headless
groups), but still read the cell's log before classifying.

### Check first

The m0 run (2026-10-22) fixed on `main`, so confirm each turned green once
released:

- moxygen: the Rust client offered `moq-00` over WebTransport, so moxygen
  picked draft-14 over draft-16. That cell passed live from `main`. The
  draft-14 QUIC cell stays red until [IETF lazy discovery](/quest/m1/ietf-lazy-discovery.md)
  lands: the CLI asks draft-14 for no namespace, so it discovers nothing
  from moxygen. A CLI subtree scope was tried and dropped, since it lost
  the sibling broadcasts a catalog references.
- js/net dialed `moqt://` as a WebTransport CONNECT with `:scheme: moqt`,
  which moq-relay refuses; the runner's moq-dev-js `remote-quic` cells
  failed with an empty message.
- `rendezvous-timeout` answered 0x10 instead of TIMEOUT (fixed in
  [moq-dev/moq#5616](https://github.com/moq-dev/moq/pull/5616) before this run).
- `invalid RESET_STREAM: 386759528` (WT_SESSION_GONE) now reads as the
  session's close.
- js/net writes each control message in one write; red5 parsed SETUP only
  from one stream frame, so recheck red5 WebTransport with js-bun.

Waiting on others:

- The runner's moq-dev client images were last built 2026-09-03 (the Rust
  one reports moq-native 0.19.15). Asked in
  [englishm/moq-interop-runner#170](https://github.com/englishm/moq-interop-runner/pull/170),
  which also declares drafts 14-22; until a rebuild, moq-dev client cells
  test stale code. [#171](https://github.com/englishm/moq-interop-runner/pull/171)
  stops the JS client's http fingerprint fetch against non-moq.dev relays.
- Reported: [moqtail/moqtail#450](https://github.com/moqtail/moqtail/issues/450)
  (draft-18 never answers SUBSCRIBE),
  [openmoq/moqx#852](https://github.com/openmoq/moqx/issues/852) and
  [gmarzot/aiomoqt#123](https://github.com/gmarzot/aiomoqt/issues/123)
  (STOP_SENDING on one subgroup ends the subscription, as
  [facebookexperimental/moxygen#252](https://github.com/facebookexperimental/moxygen/issues/252)),
  [yatagai-mm/Moqtopus#2](https://github.com/yatagai-mm/Moqtopus/issues/2)
  (closes on a request before SETUP completes),
  [meetecho/imquic#42](https://github.com/meetecho/imquic/issues/42) and
  [Eyevinn/moqlivemock#157](https://github.com/Eyevinn/moqlivemock/issues/157)
  (interop clients drop the URL path),
  [meetecho/imquic#43](https://github.com/meetecho/imquic/issues/43)
  (drops a data stream whose header spans reads; owned by
  [JS subgroup data through imquic](/quest/m1/interop-coverage/js-imquic-objects.md)),
  plus moxygen's filter type 250
  ([facebookexperimental/moxygen#225](https://github.com/facebookexperimental/moxygen/issues/225)),
  which also fails moqx's client against our relay.
- The JS publisher refuses every FETCH, so every relay that fills a late
  joiner with an upstream FETCH (imquic, MOQtail draft-22, moqt-nr draft-22,
  aiomoqt, red5) fails `js-web` publishing. Owned by
  [JavaScript FETCH](/quest/m1/js-fetch.md); recheck those cells after it
  lands before blaming the relay. MOQtail resets that subscription with
  error 0.

Unsupported or down, not worth rechecking unless something changes:
PUBLISH and SUBSCRIBE_TRACKS cases (`publish-track-*`, `namespace-discovery`,
Cloudflare's PUBLISH cases), Cloudflare draft-14 (no SUBSCRIBE_NAMESPACE, per
its feature matrix), Cloudflare draft-16 (needs a provisioned token; dropped
from the relays page), quiche-moq (connect idle timeout), xquic (its docker
relay binds loopback; its clients are raw QUIC only), and moqtopus and
xquic-draft-18 over WebTransport.

Not yet reported, for lack of a repo or a reproduction: Nokia (moqt-nr) and
MOQtail draft-22 narrowing a downstream join to Next Object with no FILL,
red5 (its repo is not public), and imquic not replaying namespaces to a later
SUBSCRIBE_NAMESPACE (passed on 2026-10-22).

Open from the m0 run: the JS subscriber still joins unfiltered before
draft-20, so a relay starting mid-group yields headless groups it stops. Next
Group Start would lose a write-once catalog without a joining FETCH, so this
waits on [JavaScript FETCH](/quest/m1/js-fetch.md).

## Related

- [Runner teardown](/quest/m3/interop-runner-approval.md) - the runner's moq.dev clients move to the withdrawing `close()` once it ships
- [Moxygen minimal interop](/quest/m1/smoke-moxygen.md) - Moxygen's filter type, waiting upstream
- [JavaScript FETCH](/quest/m1/js-fetch.md) - the JS publisher serves FETCH, which most red `js-web` publisher cells need
- [IETF lazy discovery](/quest/m1/ietf-lazy-discovery.md) - asks for watched prefixes, which the red moxygen draft-14 cell needs
