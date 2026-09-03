# [M] Mirror catalog reservation gating in @moq/hang (js/hang)

## Goal

Implement and verify the behavior tracked in [#2075](https://github.com/moq-dev/moq/issues/2075)
within the issue's stated scope and boundaries.

## Plan

Rescoped during the 2026-08 grooming: the cited js/hang/src/catalog/producer.ts
no longer exists on dev. Redesign the first-snapshot reservation gate against
dev's js/publish CatalogProducer (mutate/serve, per-subscriber seeding).

### Issue context

#### Background

[#2072](https://github.com/moq-dev/moq/pull/2072) added **publisher-side catalog reservation gating** to `moq-mux` (Rust), fixing the convergence race in #1979 where a one-shot muxer (fMP4, MPEG-TS) sees an incomplete, e.g. audio-only, catalog snapshot before a later rendition's config resolves.

The mechanism, publisher-side only (wire and catalog schema unchanged):

- `catalog::Producer::reserve()` returns a clonable `Reserved`.
- An importer reserves its rendition via `Reserved::init` (`.video()` / `.audio()`), getting a `Rendition` guard that **holds its own `Reserved` clone until its config is `set()`** (or it's dropped).
- The catalog is **withheld from the broadcast until every `Reserved` is gone**, so an unresolved rendition keeps the gate shut. When the last resolves, exactly one complete snapshot publishes. A `pending` flag avoids emitting an empty snapshot for an untouched catalog.
- Container importers own a reservation across track discovery and drop it once their initial set is declared, which also lets several importers compose into one broadcast under a single shared gate.

#### Why mirror it in JS

`@moq/hang`'s catalog `Producer` ([`js/hang/src/catalog/producer.ts`](https://github.com/moq-dev/moq/blob/dev/js/hang/src/catalog/producer.ts)) publishes incrementally as tracks are added, with no gate. A browser publisher that adds video and audio in separate ticks emits a partial catalog first, so any consumer that can't reinitialize (or just wants a stable first snapshot) hits the same race the Rust side just fixed. Parity keeps the two producers behaviourally aligned even though the wire doesn't force it.

#### Proposed direction

Add an equivalent reservation to the JS catalog `Producer` so the first published snapshot is complete:

- A `reserve()` returning a `Reserved` handle, and a per-rendition guard that holds the reservation until its config is set (or it's dropped).
- Withhold the initial publish until all reservations resolve; publish incrementally afterwards.
- Producers that don't reserve keep publishing incrementally (opt-in, non-breaking for existing callers that never call `reserve()`).

#### Design decisions to settle (why this is an issue, not a mechanical port)

- **TS shape.** Rust uses `Reserved` + `Rendition<K>` + a sealed `Kind`. TS has no importer-per-codec layer in the same shape, so decide the ergonomic surface: a `reserve()` + returned rendition handle, an options flag, or an explicit `producer.complete()` call. The `signals`/`Effect` lifecycle model may suggest a different idiom.
- **Where the gate lives.** `@moq/hang`'s publish side (`js/hang/publish`) is structured differently from Rust's container/codec importers; identify the analogous "declare all tracks, then publish" point.
- **Composition.** Whether JS needs the shared-gate-across-importers property at all, or only the single-producer complete-first-snapshot behaviour.

#### Scope / logistics

- Wire and catalog JSON schema **unchanged**  -  pure publisher-side timing.
- API addition to `@moq/hang`; if it changes existing shapes, target **`dev`** per branch-targeting rules.
- Cross-package sync: `demo/web` if it drives the catalog producer directly.

*Follow-up to #2072 (Rust). Fixes the JS half of #1979-style convergence.*

## Closes

- [#2075](https://github.com/moq-dev/moq/issues/2075) - close this issue when the quest finishes
