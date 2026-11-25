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

Recurs each major release: the PR that finishes a run keeps its results in
its own body and re-adds this file at the end of m3's `Required`, with its own `Required`
naming the [release cut](/quest/m3/release/cut.md) and Check first rewritten. Fixes land
in that PR; only maintainer decisions (public API, scope) and changes too
large to review alongside become follow-ups.

Decided with the maintainer on 2026-10-21:

- Scope: the relays page (our published Rust, browser, and Bun clients
  through every registered public relay), plus every runner cell with a
  moq.dev side: our clients, our docker relay, or `cdn.moq.dev`, including
  third-party clients against our relay. Smoke's runner job fails only on our
  clients against our relay, so this audit catches every other red cell.
- Sync new registry entries: check that new clients and docker-only relays
  show up in the runner's [report](https://git.moq.dev/interop-runner/), and
  give new relays their per-relay config (`publish_path`, `WEBSOCKET_KEYS` in
  `test/smoke/relays.sh`).
- Standing approval to open PRs on englishm/moq-interop-runner that touch only
  the `moq-dev-*` builds and registry entries, and to file reproducer issues
  on other implementations' repos. Asking upstream for anything else, such as
  restoring the commented-out image rebuild schedule, needs the maintainer.

Both reports run published packages, so verify a fix from `main`: for our
clients through a third-party relay, `CLI_BIN=<absolute path to moq> just relays
--only <key> --publishers rust --subscribers rust` from `test/smoke`; for a
third-party client through our relay, its published runner image
(`RELAY_URL`, `TLS_DISABLE_VERIFY=1`, `TESTCASE`) on the host network against
a `moq-relay` built from `main`; for the runner's own cases, its `moq-dev-rs`
or `moq-dev-js` client built against the checkout, which needs small ports to
`main`'s API. Read each cell's log before classifying, and
group cells only when their logs share a cause. A merged runner bump reaches
the nightly only after someone dispatches an image rebuild; check its GHCR
tag.

Previous runs: [moq-dev/moq#6538](https://github.com/moq-dev/moq/pull/6538),
[moq-dev/moq#7050](https://github.com/moq-dev/moq/pull/7050), and
[moq-dev/moq#7104](https://github.com/moq-dev/moq/pull/7104).

### Check first

- Most red cells were fixed on `main` and wait for the cut; confirm each
  turned green once the [runner client bump](/quest/m3/release/runner-client-bump.md)
  lands.
- Live `https://git.moq.dev/relays/results.json` baseline (2026-11-23T20:05Z,
  published 0.17/0.4 clients): green on `moq-dev-rs` (`cdn.moq.dev`),
  `stitcher-moq`, `aiomoqt-relay`, `libquicr`, `moq-rs-draft-18`, and `moxygen`.
  Red on draft-22 relays to recheck after 0.22 publishes: `oracle-relay` (newly
  registered 2026-11-17, `/interop` path fixed 2026-11-23, all 7 cells failing
  with `RequestError`/`subscribe error` on `catalog.json`), `moq-ng` (`rust -> rust`
  fails while all 5 JS pairs pass), `moqtail` (`js-web -> *` fails while
  `rust -> *` passes), `imquic` WebTransport (`rust -> *` fails while QUIC and
  `js-web -> *` pass), `red5-moq-relay` WebTransport (all JS cells fail while
  `rust -> rust` passes), and `quiche-moq` WebTransport (`js-web -> rust` invalid
  GOAWAY). `test/smoke/relays.sh` pulls `implementations.json` dynamically so
  `oracle-relay` and `red5-moq-relay` are already included.
- Ours, still red from `main`: moq-go's webtransport-go speaks WebTransport
  draft 15 (SETTINGS_WT_ENABLED and `:protocol webtransport-h3`), which
  `web-transport-proto` doesn't (check `web-transport-proto` 0.7.0); moq-dev-js
  `data-subgroup-basic` against libquicr never sees the track end; moq-dev-js
  `announce-subscribe` times out against imquic; moq-dev-js `setup-only` fails
  across many relays.
- Reported upstream, still open as of 2026-11-23 (6 earlier issues closed:
  `moqtail#450`, `moqx#852`, `aiomoqt#123`, `Moqtopus#2`, `moxygen#252`,
  `moxygen#225`):
  [cloudflare/moq-rs#281](https://github.com/cloudflare/moq-rs/issues/281) and
  [cloudflare/moq-rs#282](https://github.com/cloudflare/moq-rs/issues/282).
  Not yet filed: cloudflare/moq-rs `SubscribeRecv::error` reports a
  REQUEST_ERROR as `done` when the track has no reader.
- Unsupported or down, not worth rechecking unless something changes:
  anything past the frozen moq-transport scope `doc/concept/standard.md`
  states, including PUBLISH and SUBSCRIBE_TRACKS cases, FETCH cases with a moq-dev-js
  publisher (it refuses moq-transport FETCH, and on draft 18 a subscription
  carries its current group whole instead), relays without namespace discovery,
  peers speaking only moq-transport drafts 14 to 17, raw-QUIC-only relays
  from JS, quiche-moq, and xquic's docker relay.

## Required

- [Release cut](/quest/m3/release/cut.md) - waits for the release in flight to ship

## Related

- [Runner teardown](/quest/m5/interop-runner-approval.md) - the runner's moq.dev clients move to the withdrawing `close()` once it ships
