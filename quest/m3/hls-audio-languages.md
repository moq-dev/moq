# [M] HLS carries every audio language

## Goal

`moq import hls` publishes every `EXT-X-MEDIA` audio entry in the selected
variant's audio group as its own audio rendition, with `lang` from `LANGUAGE`
and `label` from `NAME`. The HLS and DASH exporters write those back as
`LANGUAGE` and `lang`, so a multi-language source round-trips.

## Plan

Planned 2026-11-05 (maintainer, follow-up of
[moq-dev/moq#7963](https://github.com/moq-dev/moq/pull/7963)). Today
`select_audio` (`rs/moq-hls/src/import.rs`) keeps one entry from the group
(`DEFAULT=YES`, else the first) and drops `LANGUAGE` and `NAME`; the master
playlist exporter (`rs/moq-hls/src/export/master.rs`) and the MPD exporter
(`export/mpd.rs`) write no language.

Decided 2026-11-05 (maintainer): ✅ import every audio entry, each pulled as
its own playlist, and write the language on export. Ingest then fetches
every language's segments, which is the cost of carrying them. Not chosen:
keeping single-audio import and only tagging the chosen entry.

`LANGUAGE` is already BCP-47 per RFC 8216, so it passes through. An entry
without a `URI` describes audio muxed into the variant: that muxed audio
stays one rendition (`select_muxed()`), now tagged with the entry's
`LANGUAGE` and `NAME`. A group mixing URI-less and URI-backed entries keeps
the muxed audio and adds each URI-backed entry beside it, so the
`self.audio` gate picks `select_video_only()` only when the group has no
URI-less entry.

Public API: none (the audio `label` field already exists). Wire: more
audio renditions in the catalog, and audio `lang` and `label` now set from
`LANGUAGE` and `NAME`.

## Required

- [Audio renditions carry a language](/quest/m3/audio-lang.md) - adds the audio `lang` field this fills in
