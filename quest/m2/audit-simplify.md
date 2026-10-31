# [M] Simplification audit

## Goal

The code and APIs that cost the most complexity for what they deliver are
simplified: duplicate mechanisms merged, layers, options, and generics with
one user collapsed, and features that don't earn their keep narrowed or
removed. The result is net simpler, even where that means less scope.

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

Narrowing a feature or reshaping an API is the maintainer's call, so those
are proposals with a recommendation, filed as follow-ups. Unlike the [dead
code audit](/quest/m2/audit-dead-code.md), this targets code that is used but
overbuilt.

### Check first

The m1 run (2026-10-23) ran while about 150 PRs were open, touching every
moq-net session module, the bindings, moq-tokio, moq-relay, and moq-cli, so it
skipped those. Still open from m0, unchanged because open work kept editing
the same code:

- `ietf/session.rs` `start()` builds both halves twice across the draft arms,
  and the IETF publisher/subscriber constructors drifted from lite's config
  structs.
- Lite and IETF `GroupServe` share their open, flush, and FIN-settle driver.
- `ietf/adapter.rs` reimplements `kio::Queue`; keep a drop guard for the
  writer.
- Python's `connect` forwards every `Client` parameter, still about a dozen
  after the [wrapper config](https://github.com/moq-dev/moq/pull/6507) nesting.
  Binding `connect = Client` keeps the call signature, so it needs no
  decision. Expect the wrappers' other forwarding layers to have changed too.

New from m1:

- The codec importers (H.264, H.265, AV1, VP8, VP9, AAC, Opus, MP3, FLAC, and
  legacy) each forward `demand`, `flush`, `finish`, `abort`, `cut`,
  `discontinuity`, and `seek` to their container producer. Dropping the
  forwards means exposing a shared track handle, which is an API proposal. The
  audio and video producers differ in their config type, so check the
  handle can be one type before proposing it.
- moq-mux's H.264 and H.265 `Import` and `Split` share their shape but differ
  in SEI scanning, open-GOP recovery points, and error variants. A generic
  version needed enough hooks that it was left alone; recheck it if those
  converge.

Proposals from m0 are settled. The moq-video raw-frame publisher and the
shared `Background` would each have had moq-ffi as their only user once the
[generated C bindings](/quest/m2/c/README.md) retire moq-c's hand-written
copies, so both were dropped. The Go wrapper's missing `bridge.CallHandle` was
in the generated binding, not the wrapper: uniffi-bindgen-go
`v0.10.0-kixelated.2` destroys a result whose context was cancelled as it
completed.

### Results (m1, 2026-10-23)

One PR, private and behavior-preserving: moq-mux's `import::Track` and
`TrackStream` expand their codec arms through one macro each, and nine
importers lost a deleted method's doc left on `cut`. The H.264 and H.265
exporters were left alone for [one shared Annex-B export lifecycle](https://github.com/moq-dev/moq/pull/6575),
which has since landed. Search the
quest tree before fixing a duplicate: the 2026-10-22 refactor audit filed
several.

### Kept on purpose

- Backend and playback traits have several implementations or device-free
  test doubles; catalog traits erase application extension types.
- The ietf publisher's `RequestWaker` and lite's `FetchWake` stay apart from
  the queue-once waker: one collects into a list and wakes an owner, the
  other fills a set.
- js/net's subscriber grant checks stay hand-written: `auth_gate.ts` `Gate`
  only checks publish grants, and a direction flag costs what it saves.
- The JS datagram send and receive loops: the lite and IETF error handling
  and `served` bookkeeping differ, so a shared loop needs hooks.
- Dart and C++ wrappers are already thin. The Swift wrapper's forwarding
  classes stay classes, for argument labels and unprefixed names.
- Kotlin's `AnnounceConsumer.updates()` cancels on every completion, as its
  doc promises.
- The `until_wanted` loop copied into moq-audio, moq-boy, moq-video, and
  moq-transcode is one line over `Demand::wanted_changed`. A helper on
  `Demand` would be a second way to wait for the one demand signal.
- js/net's lite version gates mirror Rust's on purpose, and its per-message
  IETF error classes are distinct wire messages.

Transport adapter deduplication and compression removal have dedicated work;
avoid a second design here. Start with the items above, then the largest
modules again, and the bindings' forwarding layers. The largest moq-net
modules at the end of m1: `ietf/publisher.rs` 17,182 lines,
`ietf/subscriber.rs` 17,066, `model/origin.rs` 13,036, `lite/publisher.rs`
8,508, and `lite/subscriber.rs` 7,839. The three m0 named all grew.

## Required

- [Release cut](/quest/m2/release/cut.md) - wait for the release in flight to ship
