# [S] Stats frames carry each counter under one name

## Goal

A stats frame writes each counter once, under its current name
(`announces_started`, `sessions_ended`, and so on). The eight spellings from
before the May 2026 rename are neither written nor read, in moq-net or in
`@moq/stats`, and `Traffic` and `Presence` derive their serde shapes instead
of hand-writing a second copy of them. The track names, the remaining
fields, and the collection stay as they are.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ Drop the old spellings, derive the serializers, and remove the
  `@moq/stats` fallback (rejected: keeping them one more release). The
  format is documented well enough for others to read, so what the cut
  ships is a contract, and nothing can reach the old names any more.

Not picked, recorded so they are not reopened without new evidence: renaming
`wire_tx` and `wire_rx` to match `sent_bytes` and `received_bytes` (a
downstream reader already names them), and moving the relay's admission
counters (`RefusalKind`, `Refusals`, `Handle::refuse`) out of moq-net.

Facts found by the audit:

- `Traffic` writes six counters twice and `Presence` two:
  `announces_started` and `announced`, `announces_ended` and
  `announced_closed`, `broadcasts_*`, `subscriptions_*`
  (`rs/moq-net/src/stats.rs:548-656`), and `sessions_started` and
  `sessions`, `sessions_ended` and `sessions_closed` (`:735-825`). Each has
  a hand-written `Ser` and `De` twin, with `counter_edge` (`:544`)
  preferring the current name on read. About 220 lines.
- `@moq/stats` carries the same fallback as `legacy()`
  (`js/stats/src/frame.ts:9-31`, `:74-75`). The package is at 0.0.1 and has
  never been published.
- `release` already writes both spellings and prefers the current one
  (`git grep -c announces_started origin/release -- rs/moq-net/src/stats.rs`
  gives 30), so a reader built from this cut reads a `release` relay
  without the shim, and the two newest releases interoperate as relays.
- The track layout changed in the window
  ([moq-dev/moq#6428](https://github.com/moq-dev/moq/pull/6428)): `release`
  publishes `[<tier>/]{publisher,subscriber,sessions}.json[.z]`, and `main`
  publishes `totals.json`, `bucket.<epoch>.json`, `presence.json`, and
  `prefix.json` and refuses other names. A reader old enough to know only
  the old spellings cannot find a track on `main` at all, so the duplicate
  fields reach nobody.
- `rs/AGENTS.md` asks for derived wire shapes over hand-written copies.

For the PR:

- Delete `TrafficSer`, `TrafficDe`, `PresenceSer`, `PresenceDe`, `Edge`,
  and `counter_edge`; derive `Serialize` and `Deserialize` on `Traffic` and
  `Presence` with `#[serde(default)]`, so a missing counter still reads as
  zero and an unknown one is ignored, as today.
- Delete the tests that pin the old shape (`older shape parses`,
  `counter_edge_*`, around `stats.rs:3091` and `:3649-3702`) and keep one
  that a frame round-trips and that a frame missing a counter reads zero.
- `@moq/stats`: delete `legacy()` and its two uses; the schemas keep their
  zero defaults.
- Check `rs/moq-stats`, `rs/moq-relay` (`/metrics`), and
  `doc/concept/stats.md` for an old name; the doc should list each counter
  once.
- Before merging, confirm with moq.pro's readers (dashboard and billing)
  that none reads an old name. One that does moves in the same deploy; say
  so in the PR's Impact section.

Tests: the round trip above in Rust and JS, and `just test interop` if a
stats frame crosses it.

Public API: none in Rust (the serde shape of two public types narrows).
Wire: the stats JSON loses eight duplicate fields; nothing is renamed.

## Related

- [Wire stats](/quest/m3/wire-stats/README.md) - adds counters to the same frames after the cut
