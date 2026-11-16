# [S] One rule refuses a mis-shaped catalog section

## Goal

One rule decides what a hang catalog section with the wrong shape means: a
root key the hang draft defines has its shape, or the catalog is refused.
An application keeps its own root data under a namespaced key. `rs/hang`
and `js/hang` implement the one rule, and the draft states it once. The
draft's `delay` definition, its worked example, and the Rust and JS doc
comments for `delay` and `jitter` describe what the publisher measures, and
the stale passages in the hang draft are gone.

## Plan

Decided 2026-11-16 (maintainer, /quest-plan of the pre-cut audit):

- ✅ One strict rule (rejected: one lenient rule for every key; keep the
  per-key rules). Fail loud: a section that silently reads as empty hides a
  publisher bug.
- ✅ The draft adopts the code's `delay` rule, with a corrected example and
  doc comments (rejected: the code returns to the maximum shortfall). The
  code's rule is the one that was measured.

Facts found by the audit:

- A mis-shaped section is, by key: refused (`video`, `audio`, `include`,
  `m2ts`, `version`); empty unless it is an object (`media`,
  `deserialize_media`); empty unless it has its map key (`text`, `json`,
  `binary`, `deserialize_section`); absent unless it has a string field
  (`stats`, `echo`, `deserialize_named`); absent on any error
  (`media.join`, `DefaultOnError`). The decoders are in
  `rs/hang/src/catalog/mod.rs`, mirrored by `section`, `named`, and
  `object` in `js/hang/src/catalog/section.ts`.
- The lenient rules exist because an application "may already carry" the
  key. `media`, `stats`, and `echo` are new since hang-03, added in the
  same window as "an application SHOULD name its own root sections with a
  namespaced key", and `include`, also new, got the strict rule. `text`,
  `json`, and `binary` are in hang-03's root schema, so they were already
  reserved.
- `delay` is defined three ways. The draft: "the maximum amount" a frame
  trailed another rendition, with an example whose answer is 20. The code
  since [moq-dev/moq#7689](https://github.com/moq-dev/moq/pull/7689): the
  least a frame trailed within a window, and the largest of those windows
  (`rs/moq-mux/src/catalog/estimate.rs`, "a window's delay is the least of
  those trails"), which gives 0 on the draft's example. The JS comment
  (`js/hang/src/catalog/video.ts`): "behind the broadcast's earliest
  rendition".
- `jitter`: the draft says a frame written in presentation order
  contributes none "regardless of frame rate", while
  `rs/hang/src/catalog/video/mod.rs` and `video.ts` still say "if each
  frame is flushed immediately, this would be 1000/fps".

For the PR:

- Rule: `stats`, `echo`, `text`, `json`, and `binary` decode strictly.
  Delete `deserialize_section`'s and `deserialize_named`'s fallbacks and
  their JS twins, keep `deserialize_object`, and replace the draft's
  per-key "is ignored" and "treat as absent" sentences with one statement
  in the Root section. [The sync section](/quest/m2/hang04/sync.md) and
  [the MPEG-TS section](/quest/m2/hang04/m2ts.md) own their keys; whichever
  lands second meets the rule already in place.
- Before deleting a fallback, search the tree and the docs for an
  application that writes its own value under one of those keys
  (`js/room` writes a root `hang` section, which no rule here touches).
  One found in a published package is the evidence that would keep that
  key lenient; record it.
- `delay`: rewrite the field section to the per-window least and the
  largest window, with an example that yields a nonzero value under that
  rule, and align the "never subtract one rendition's `delay` from
  another's" sentence. Fix the `delay` and `jitter` doc comments on audio
  and video in both languages.
- Stale draft text, none of which changes the format: delete Discovery
  (it uses `ANNOUNCE_PLEASE` and `active=true`, which lite-07 does not
  have, and names rooms `/room/alice.hang` where the Rooms section and
  `js/room` use `{identity}/camera.hang`); the abstract's "real-time
  conferencing protocol" becomes what the document is, the media format
  every gateway shares; `JsonSchema` and `BinarySchema` gain the `instance`
  the code carries.
- Not here: the duplicated MPEG-TS SI section goes with
  [the MPEG-TS section](/quest/m2/hang04/m2ts.md), the dual-decoder
  sentences with [video warmup](/quest/m2/hang04/video-warmup.md), and the
  marker rules with
  [media consumers](/quest/m2/time-addressing/consumers.md).

Tests: for each key, a wrong-shaped value refuses the catalog in both
languages (the existing `a_foreign_media_value_is_ignored_but_a_bad_clock_refuses`
style tests flip); `parity.test.ts` covers the same documents.

Public API: none. Wire: the reading rule for a catalog that is already
wrong; a document that read as "section absent" is now refused.
