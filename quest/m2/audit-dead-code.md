# [M] Dead code and API surface audit

## Goal

Code, exports, Cargo features, and CLI flags that nothing uses are deleted, or
are planned for deletion when removal breaks a public API.

## Plan

Recurs once per major release (decided 2026-10-19). The PR that finishes
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

Removing a public item is a breaking change and the maintainer's call, so it is
always a follow-up, never an inline fix.

The m1 run (2026-10-20) removed unused `@moq/signals` dependencies from
`@moq/json` and the browser bench, the web demo's unused `solid-element`, and
module-local exports in the carla web demo and `js/net`'s IETF
`MAX_AGE_OPTION` and `finCancels`. No Rust code, Cargo feature, or flag was
removed. It ran while the lint audit's PR rewrote every Rust suppression,
so the dead-code attributes were counted but left alone.

Start by comparing the live tree against these baselines:

- Rust had 423 dead-code/unused suppression lines: 77 unconditional, 346
  through `cfg_attr`. The unconditional ones are drop guards, compile-time
  Send assertions, generated settings registries, shared test and benchmark
  harnesses, deserialization fixtures, FFI tables, and
  `group::Producer::abort_unused`, which only tests call since the answering
  API that wanted it was dropped.
  Once the lint audit lands, every unconditional one is an `expect`, so a
  stale one fails the build; check what the conditional ones still guard.
  `cargo shear` found no unused dependencies.
- Rustc misses unused `pub` items. Tokenizing the tree and keeping `pub`
  declarations whose name appears exactly once (`.tmp` script, not
  checked in) found, outside generated bindings:
  - cfg(test) helper `track::Subscriber::assert_error` in moq-net's model,
    since removed. `AnnounceConsumer::assert_next_restarted`/`assert_next_ended`
    also matched, but `model/origin.rs` calls them.
  - Public items no consumer calls: `moq_net::Version::uses_setup_negotiation`
    (since removed), `moq_tokio::Server::serve_both`, `quic::web` `ClientBuilder::with_server_certificates`
    and `ServerBuilder::with_certificate`, `hang` `Catalog::to_json_pretty`,
    `moq_audio::Format::is_planar`/`is_float`, and the vendored forks
    (`moq-quic`, `moq-sock`, `moq-v4l`) that keep upstream's surface.
  Rerun it first; a name used only by its definition is the shortlist.
- Knip 6.42.0 with explicit entries for Bun tests, `*.browser.ts`, benches,
  examples, worklets, TSX, every `test/*` client, the repo-root scripts, and
  `ignore` for `rs/**/test_data` reports only real findings plus root
  devDependencies used as CLIs and `tsx`/`wrangler` run by scripts. Preserve the
  TypeScript DOM override and `rolldown` wherever a Vite config loads
  `js/common/vite-plugin-worklet.ts`. `doc/concept/audio-jitter/reference.ts`
  exports constants only it uses; it was skipped because an open PR rewrote
  it.
- There were 150 Cargo feature declarations across 27 manifests. Only three
  have no in-tree user: the moq-native tombstone's `android-logcat`, the QUIC
  FIPS crypto feature, and moq-cli's `nvenc`/`nvdec` aliases, since removed
  like moq-video's and moq-transcode's.
- The settings registries declared 120 distinct long flags, 186 counting
  every `usage` declaration. Each one reaches a runtime consumer; the
  `--client-*`/`--server-*` QUIC and TLS spellings and the relay's legacy auth
  fields are hidden deprecated spellings that fail with migration guidance.
  Trace a flag through the config merge, not through prose or examples.

No public removal landed in this run itself; the items marked removed above
went in a follow-up PR. Propose any public removals separately with their migration and consumer evidence. The public model and
performance audits own their API and benchmark decisions.

From the 2026-10-25 run:

- moq-tokio's `RecvBandwidth::poll` still handles a closed output, but `out`
  can no longer close while that call borrows it
  ([moq-dev/moq#6744](https://github.com/moq-dev/moq/pull/6744)).
- Locals and fields still named `dynamic` after the `requests()` rename
  ([moq-dev/moq#6752](https://github.com/moq-dev/moq/pull/6752)).

## Required

- [Release cut](/quest/m2/release/cut.md) - the release in flight ships before the next audit starts
