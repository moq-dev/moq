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
code audit](/quest/m1/audit-dead-code.md), this targets code that is used but
overbuilt.

Starting points (2026-10-19): look for one concept implemented twice (across
crates, or Rust and JS drifting apart), traits and generics with a single
implementation, config knobs every caller sets the same way, wrappers that
only forward, and the largest modules and functions. Prefer reusing what the
stack already has over a parallel mechanism.

Progress (2026-10-19): the JSON stream consumer reuses the byte stream
consumer's single-group lifecycle instead of maintaining its own receive,
roll detection, and teardown state machine. JSON validation, error identity,
and concurrent-read refusal stay at the JSON boundary. Its record count,
payload size, and compression sweep runs in the JavaScript nightly lane.

The initial scan covered private Rust traits, media/container dispatch,
transport adapters, JSON codecs, and duplicated JavaScript utilities.
Backend and playback traits have multiple implementations or device-free
test doubles; catalog traits erase application extension types. Keep those
seams. Transport adapter deduplication and compression removal already have
dedicated work; avoid a second design in this audit.

Second checkpoint (2026-10-19): the large publisher/subscriber modules and
binding layers are reviewed. Landed, all private: one IETF request-error
writer in Rust and JS (draft-14 bytes pinned per request kind), Rust's lite
track-stream and GOAWAY gates mirrored in JS, one moq-c callback task slab and
spawn helper, one moq-c media consume path, one Kotlin pull adapter, one
Python async stream base, and small Go duplicates. Dart and C++ wrappers are
already thin.

Still to do before completing the run, each waiting on the named open work:

- `ietf/session.rs` `start()` builds both halves twice across the draft arms,
  and the IETF publisher/subscriber constructors drifted from lite's config
  structs. Wait for the GOAWAY, close-drain, and receive-credit PRs that edit
  `start()`.
- Lite and IETF `GroupServe` share their open, flush, and FIN-settle driver.
  Wait for compressed tracks.
- `ietf/adapter.rs` reimplements `kio::Queue`; keep a drop guard for the
  writer. Wait for the GOAWAY PR that edits the same impl.
- Keyed "mark ready and wake" wakers repeat in `lite/subscriber.rs`,
  `ietf/publisher.rs`, and `model/origin.rs`.
- JS: hand-built "wake on any of these signals" waits can use
  `Signal.race`, armed before reconciling; subscriber authorization could
  reuse `auth_gate.ts` `Gate`.
- moq-c: the session, server, dynamic, request, and raw slabs still wrap
  entries in an `Option` that is never `None`.

Maintainer proposals, not changes: a moq-video raw-frame publisher both
bindings wrap (moq-c's copy also runs its bandwidth follower on the C
runtime, which may stall it), moving the bindings' duplicated `Background`
into moq-tokio, typealiasing the Swift wrapper's forwarding classes, and
making Python's `connect` the `Client` class. Kotlin's `AnnounceConsumer`
flow cancels on every completion while the others cancel only on
cancellation; unifying it is a behavior call.
