# [M] Audio renditions carry a language

## Goal

A publisher can mark each audio rendition with a BCP-47 `lang`, and a player
can build an audio-language menu from the catalog. Watch's automatic pick
stays within one language instead of choosing between languages by bitrate,
and the docs show how to publish multi-language audio and text tracks.
Importers filling in `lang` are out of scope.

## Plan

Reported in [moq-dev/moq#7945](https://github.com/moq-dev/moq/issues/7945).
Today audio renditions have no language field in either catalog
(`js/hang/src/catalog/audio.ts`, `rs/hang/src/catalog/audio/mod.rs`), while
text renditions already have `lang` (`js/hang/src/catalog/text.ts`). Watch
picks from every enabled audio rendition together (`#select` in
`js/watch/src/audio/source.ts`, the first by container kind), so with two
languages it takes whichever comes first. `controls.audio.target` can already
pin a rendition by name. The hang-to-MSF export sets `altGroup = 1` on all
audio once a broadcast has more than one audio rendition
(`rs/moq-mux/src/catalog/producer.rs`), marking languages as switchable
bitrates, and the MSF-to-hang consumers keep `lang` for text only.

Decided 2026-11-05 (maintainer, /quest-import):

- ✅ Scope: catalog field, language-aware selection, MSF mapping, docs. Not
  chosen: also a built-in Audio tab in watch's settings panel (the reporter
  builds their own menu), an epic that plans the importers now, or the field
  alone (leaves auto-pick choosing languages by bitrate).
- ✅ The field is optional `lang`, matching text renditions and MSF, in the
  JS and Rust catalogs and the JS publish config. Old clients ignore it.
- ✅ With no viewer choice, watch matches `navigator.languages` against the
  renditions' `lang`; with no match it takes the top-ranked rendition's
  language. It then ranks within that language as today. The audio `Target`
  can also select by `lang`. Not chosen: a new publisher `default` flag (one
  more field to carry through MSF and HLS).
- ✅ MSF export groups audio by `lang` (one `altGroup` per language), and both
  MSF consumers (Rust and `js/watch/src/msf.ts`) keep audio `lang`.
- ✅ The docs change shows publishing several audio languages and a text
  track with `broadcast.text(name)`, which the reporter couldn't find.

Public API: additive `lang` on the audio catalog types and the audio target.
Wire: an optional catalog field; MSF `altGroup` changes for multi-language
audio.

## Closes

- [#7945](https://github.com/moq-dev/moq/issues/7945) - support multi audio language renditions

## Related

- [Gapless audio switch](/quest/m3/gapless-audio-switch.md) - a language switch is a rendition switch and inherits its gap
