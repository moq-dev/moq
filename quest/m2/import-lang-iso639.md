# [S] fMP4 and TS imports publish BCP-47 languages

## Goal

`moq import` of fMP4 and TS sources sets `lang` (fMP4 audio and text, TS
audio; TS imports publish no text renditions) to a BCP-47
tag that browsers' `navigator.languages` can match: the ISO 639-1 code where
one exists (`en`, not `eng`), else the three-letter code. `und` and absent
codes leave `lang` unset.

## Plan

Planned 2026-11-05 (maintainer, follow-up of
[moq-dev/moq#7963](https://github.com/moq-dev/moq/pull/7963)). fMP4 audio
drops `mdhd.language`, and fMP4 text copies it raw
(`rs/moq-mux/src/container/fmp4/import.rs`, `config.lang = Some(...)`), so
text publishes `eng` and `und`. TS copies ISO_639_language_descriptor (0x0A)
bytes only into the `mpegts` catalog extension
(`rs/moq-mux/src/container/ts/catalog.rs`, parsed in `psi.rs`), and nothing
reads a language from them.

Decided 2026-11-05 (maintainer):

- ✅ One quest for fMP4 and TS: both carry ISO 639-2 codes and share the
  mapping. Not chosen: a quest per container.
- ✅ Map with the `isolang` crate (both the /B and /T codes to 639-1, else
  keep the three-letter code). `isolang` knows only 639-3 (= /T for these
  codes), so the ~20 /B codes (`fre`, `ger`, `dut`, `chi`, ...) normalize to
  /T through a small alias table first; an unmapped code never publishes a
  /B form. An fMP4 `elng` box, already BCP-47, wins over `mdhd`; mp4-atom
  0.17 does not decode `elng`, so this quest adds it to `Mdia` upstream. Not chosen: a hand-written table (data to keep correct), or
  publishing codes raw and matching 639-2 in every player.

Public API: none. Wire: fMP4 text `lang` changes from `eng` to `en` and
from `und` to unset; fMP4 audio and TS audio `lang` become set where they
were unset.

## Required

- [Audio renditions carry a language](/quest/m2/audio-lang.md) - adds the audio `lang` field this fills in
