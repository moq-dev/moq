# [M] Give JSON and binary wrappers consistent configuration

## Goal

Switching JSON/binary modes does not unexpectedly change constructor shape,
and a consumer's config does not accept producer-only knobs that do nothing.

## Plan

At dev `e2350b39a`, JSON Snapshot Producer alone takes `{ track, ... }`
(`js/json/src/snapshot/producer.ts:7,27`); other producers and consumers take
`(track, config)`. The snapshot move was intentional in #2393 (`d7160da73`),
so account for that rationale. Snapshot Consumer also takes encoder Config
(`snapshot/consumer.ts:22`), accepting `initial` and `deltaRatio` although its
decoder only reads schema and compression (`snapshot/decoder.ts:28`).

Decided: standardize track-owning wrappers on an options object, preserving
the deliberate snapshot choice. Separate producer and consumer options and
keep bare codecs separate from track ownership.

Migrate all JSON/binary modes and imports, docs, demo recipes, and the external
consumer fixture together. Do not add old constructor aliases. Add negative
type coverage for producer-only consumer options and check emitted package
declarations. Existing functional tests should continue proving codec behavior.
Schema validation for stream/window is a separate capability decision; do not
silently expand this quest into changing their data model.

Public API: breaking constructor/config types. Wire: none. Run JS check/test.

## Related

- [Wrapper lifecycle](/quest/m1/api-js-wrapper-lifecycle.md) - independent ownership behavior on the same wrapper family
