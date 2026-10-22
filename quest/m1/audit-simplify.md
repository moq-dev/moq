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

Left from the m0 run because open work edited the same code (close-tail drain
landed in [#5762](https://github.com/moq-dev/moq/pull/5762)):

- `ietf/session.rs` `start()` builds both halves twice across the draft arms,
  and the IETF publisher/subscriber constructors drifted from lite's config
  structs.
- Lite and IETF `GroupServe` share their open, flush, and FIN-settle driver.
- `ietf/adapter.rs` reimplements `kio::Queue`; keep a drop guard for the
  writer.

Maintainer proposals from m0, not yet planned, each with its recommendation:

- Yes: a moq-video raw-frame publisher that moq-ffi and moq-c both wrap,
  removing about 250 duplicated encode and bandwidth-follow lines. moq-c's
  copy calls `pollster::block_on(set_bitrate)` on the C runtime while holding
  the encoder lock, which may stall other moq-c tasks (unverified).
- Yes, after the publisher: move the bindings' duplicated `Background` into
  moq-tokio, with a blanket `Driver for &mut D` in moq-net.
- Yes: make Python's `connect` the `Client` class, dropping a 27-parameter
  forwarding signature.
- No: typealias the Swift wrapper's forwarding classes. The wrapping is
  deliberate (argument labels, no `Moq` prefixes), and it would break Swift.
- Keep: Kotlin's `AnnounceConsumer.updates()` cancels on every completion
  while the other flows cancel only on cancellation; its doc promises that.

The Go wrapper's `Server.Accept` and handle-returning methods skip
`bridge.CallHandle`, which may leak an accepted handle on cancellation. This
is unverified and worth its own investigation.

### Results (m0, 2026-10-20)

Three PRs, all private and behavior-preserving:
[#5977](https://github.com/moq-dev/moq/pull/5977),
[#6049](https://github.com/moq-dev/moq/pull/6049), and this run's final PR.
They merged the JSON stream consumer into the byte stream consumer, one IETF
request-error writer in Rust and JS, Rust's lite version gates mirrored in
JS, one moq-c task slab with no never-`None` entries, one moq-c media consume
path, one Kotlin pull adapter, one Python async stream base, small Go
duplicates, one queue-once keyed waker in moq-net, and one armed change wait
in js/net's publishers.

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
- Dart and C++ wrappers are already thin.

Transport adapter deduplication and compression removal have dedicated work;
avoid a second design here. Start with the items above, then the largest
modules again, and the bindings' forwarding layers. The largest moq-net
modules at the end of m0: `ietf/publisher.rs` 16,047 lines,
`model/origin.rs` 11,863, and `lite/subscriber.rs` 7,181.
