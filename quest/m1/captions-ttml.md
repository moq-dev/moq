# [M] TTML caption import

## Goal

Importing an fMP4 `stpp` track within a supported TTML/IMSC profile yields a `ttml` text
rendition whose cues stay on the shared media clock. Today those XML tracks are
skipped with a warning while SRT and WebVTT import as `vtt`.

## Plan

Copying XML bytes is not enough. MP4 `stpp` names an XML namespace, which is not
necessarily TTML, and the importer shifts the source clock, so every TTML time
expression must be rewritten onto the catalog clock.

- Support a bounded TTML/IMSC timing profile: validate the namespace and refuse
  anything outside the profile instead of relabelling arbitrary XML as TTML.
- Convert every time expression, including nested `begin`/`end`/`dur` and
  `ttp:timeBase` variants the profile admits, after the clock shift.
- Keep skipping XML tracks the profile does not cover, one track at a time.

Matroska has no registered TTML codec ID (the old `S_TEXT/TTML` claim was
removed), so confirm a real mapping before adding one.

Cover a namespace mismatch, nested timing, and a shifted source clock.
