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
- Ours, still red from `main`: moq-go's webtransport-go speaks WebTransport
  draft 15 (SETTINGS_WT_ENABLED and `:protocol webtransport-h3`), which
  `web-transport-proto` doesn't; moq-dev-js `data-subgroup-basic` against
  libquicr never sees the track end; moq-dev-js `announce-subscribe` times out
  against imquic; moq-dev-js `setup-only` fails across many relays.
- Reported upstream, search before filing again:
  [cloudflare/moq-rs#281](https://github.com/cloudflare/moq-rs/issues/281),
  [cloudflare/moq-rs#282](https://github.com/cloudflare/moq-rs/issues/282),
  [moqtail/moqtail#450](https://github.com/moqtail/moqtail/issues/450),
  [openmoq/moqx#852](https://github.com/openmoq/moqx/issues/852),
  [gmarzot/aiomoqt#123](https://github.com/gmarzot/aiomoqt/issues/123),
  [yatagai-mm/Moqtopus#2](https://github.com/yatagai-mm/Moqtopus/issues/2),
  [facebookexperimental/moxygen#252](https://github.com/facebookexperimental/moxygen/issues/252),
  and [facebookexperimental/moxygen#225](https://github.com/facebookexperimental/moxygen/issues/225).
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
