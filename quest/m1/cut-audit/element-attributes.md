# [S] Media element attributes reflect their state

## Goal

`<moq-publish>` and `<moq-watch>` reflect state changed through properties
or the built-in UI back to their attributes, so `[muted]` selectors,
attribute-diffing frameworks, and a later `setAttribute` behave. Invalid
attribute values warn instead of being ignored.

## Plan

Found by the 2026-10-24 pre-cut audit at 151a529670. Source-inferred,
inherited. Public element API, so it lands before the cut.

- Nothing reflects on publish (`js/publish/src/element.ts:337`, `:483-529`).
  When the built-in UI unmutes via the property, the `muted` attribute stays,
  so a later `setAttribute("muted", "")` hits the `oldValue === newValue`
  early return and the mic stays live.
- On watch, `captions`, `announced`, and `catalog-format` do not reflect
  (the UI writes `controls.text.target` in `ui/components/captions.ts:58`),
  and `parseCatalogFormat` (`watch/src/element.ts:109`) silently ignores an
  invalid value.
- `MoqWatch` exposes `broadcast`, `video`, `audio`, `renderer`, and `sync`
  as reassignable fields; make them `readonly`.

Add reflection effects as watch already does for its other attributes.

Public API: additive reflection; `readonly` fields reject reassignment.
Wire: none.
