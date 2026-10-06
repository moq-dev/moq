# [L] Delete the announce live marker

## Goal

Announce streams yield only route events (start, update, end) in
`rs/moq-net`, `@moq/net`, moq-ffi, and every binding. The `Live` marker and
everything that exists to produce it are deleted: the per-session replay
holds and landing bookkeeping on the origin, the quiet-period guesses on
wires without an exact count, and the marker in the bindings' event types.

## Plan

Decided 2026-09-29 by the maintainer, after walking through a page-load fix,
a Live/Offline toggle, and a per-session `live()`:

- The marker answers "has the initial list arrived?" Only one-shot listing
  (`moq ls`, shell completion, both since removed) used it, and no app does: room, watch, and the
  demo skip it. An origin merges many sessions and local publishers, so
  "caught up" there needs aggregation across connections that start, fail,
  and reconnect independently. That produced the page-load race, the special
  first connection, and the offline question. With no customer, delete it
  rather than patch it.
- No replacement for now. A single-connection listing (a `session.live()`
  returning the peer's initial set) can come back when a customer needs it.
- Apps drive loading and offline UI from connection status, which `@moq/net`
  already exposes. No empty-state work is planned.
- Wire unchanged. lite's ANNOUNCE_OK Active Count and the IETF Active Count
  extension (`drafts/draft-lcurley-moq-active-count.md`) stay on the wire,
  unused; a future listing would read exactly that count. Peers still send
  and validate it. Only the local consumption goes.

Guidance:

- The marker is unreleased, on `main` only (Rust since #4059, JS since #4261, bindings
  since #4266), so this deletion breaks nothing published.
- Look for code that only exists to produce the marker, and delete it rather
  than stubbing it: replay/landing counters in the origin, holds taken by the
  JS forwarder and reconnect loop, quiet-gap timers on lite-03/04 and
  moq-transport, and the batching of the initial set if nothing else needs
  it. Keep what other features rely on (for example, request answering
  through `expect()`).
- Update `doc/lib/{rs,js}`, `doc/concept`, the binding docs, and examples
  (`js/net/examples/discovery.ts`) inline. Delete marker-only tests.
- Run `just test interop --all`.

Public API: removes `AnnounceEvent::Live` / `{ kind: "live" }` /
`MoqAnnounceEvent::Live` and the binding aliases. Wire: none.
