# [S] Unused crates, scripts, and files are deleted

## Goal

Code and files with no consumer, whose removal breaks no published API, are
gone: the `moq-room` crate, the `moq-native` tombstone, `demo/throttle`,
`sh/dart/publish.sh`, `demo/web`'s Solid dependencies, and release notes for
packages that never publish.

## Plan

Planned 2026-11-09 (maintainer, /quest-plan from the deletion audit).
Decided: ✅ delete, in one PR with one commit each (rejected: keep them for
possible future use).

- `rs/moq-room`: `publish = false` since
  [moq-dev/moq#6454](https://github.com/moq-dev/moq/pull/6454), no
  workspace or binding consumer, and `doc/lib/rs/moq-room.md` already points
  users to `@moq/room`. Delete the crate, its workspace entry, and its doc
  page, and rewrite [room alias withdraw](/quest/m3/room-alias-withdraw.md)
  to JS only. `@moq/room` stays.
- `rs/moq-native`: the 0.20.0 tombstone has been on crates.io since
  2026-09-23 and stays there. Remove the crate and its workspace entry so the
  shared-minor cut cannot publish another tombstone. The redirects in
  `doc/worker.js` stay.
- `demo/throttle`: a macOS-only pf/dummynet script behind `just throttle` in
  `demo/justfile`; moq-shaper does seeded, cross-platform impairment.
- `sh/dart/publish.sh`: no caller; the release workflows run
  `dart pub publish` inline.
- `demo/web`: `solid-js`, `vite-plugin-solid`, and `solidPlugin()` with no
  `.tsx` in the demo.
- Release notes for packages that never publish: `demo/boy/CHANGELOG.md`,
  `js/p2p/MIGRATE.md`, `js/crypto/MIGRATE.md`, `rs/moq-quic/MIGRATE.md`
  (157 lines), and the "unpublished API" paragraph in `rs/moq-congestion`'s
  README. A package gets them when it first publishes.
- `rs/moq-rtmp/src/flv.rs` matches `moq_mux::Error::TimestampRewind` to
  decide which demux failures end the session, against the rule never to
  match on errors. Make the importer report that outcome as its own result
  instead; how is the implementer's call.

Kept on purpose (2026-11-09): `Hang.Health` and `hang::health` have no
caller in this repository, but moq.pro's QoS line plans to use the client
health sample and classification.

Public API: none (moq-room and moq-native no longer publish). Wire: none.
