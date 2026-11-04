# [S] Catalog contributions become includes on the root's timeline

## Goal

The hang catalog's root `contributions` map is renamed to say what it is: other
broadcasts the root includes, which share the root's PTS timeline and its
catalog-root settings (clock, join interval). The rename covers the Rust and JS
catalog types and schemas, `@moq/watch`'s `broadcast.contribution(effect, id)`,
the parity fixture, `doc/concept/hang.md`, and the draft's
`{#contributions}` section, with the shared-timeline rule stated in the draft.
Done before the next release cut, while the field is still unreleased.

## Plan

Planned 2026-11-04 with the maintainer, while settling
[moq-dev/moq#7836](https://github.com/moq-dev/moq/pull/7836)'s HLS listing.
The maintainer: keep contributions; "consider renaming them. Afaik the
use-case is moq.pro transcodes so they must share PTS and really everything in
the catalog root. Its why I wanted to call the section `include` originally."

- ✅ **Contributions stay** (maintainer, 2026-11-04). Not chosen: removing them.
- ✅ **They share the root's timeline** (maintainer, 2026-11-04): consumers
  such as moq-hls treat a contribution as on the root's PTS timeline and grid,
  with no clock or interval comparison; a mismatch is the publisher's bug.
  #7836 implements that for HLS.
- **The name is the open decision.** `include` is the maintainer's original
  preference; confirm it (or another) in this quest's PR before renaming, then
  rename the field, the types (`Contribution`), the watch method and the draft
  section consistently in Rust, JS and the bindings that expose them.
- The field arrived in [moq-dev/moq#7312](https://github.com/moq-dev/moq/pull/7312)
  (2026-11-01) and isn't on `release`, so no compatibility alias is needed and
  the catalog `version` stays.
- Update the [HLS contributions](/quest/m0/hls-contributions.md) quest, or its
  code if #7836 has landed, to the new name.

Public API: breaking but unreleased (Rust `hang`, `@moq/hang`, `@moq/watch`).
Wire: the catalog JSON key changes; unreleased.

## Related

- [HLS lists contributions](/quest/m0/hls-contributions.md) - the HLS consumer settled with the shared-timeline rule
