# [M] Caption import

## Goal

Importing a file with subtitles yields a text rendition instead of an error or
a dropped track. A broadcast with captions currently loses them at every
container boundary.

## Plan

Two importers refuse timed text today:

- fMP4 returns `Error::UnsupportedSubtitle` for an `sbtl` handler, so a file
  with a subtitle track fails the whole import rather than importing the media
  it does understand.
- MKV logs and drops them, alongside unsupported codecs.

Import both as `text` renditions. Map each source track to a `TextFormat`
(`wvtt` sample entries and MKV's `S_TEXT/WEBVTT` are WebVTT; `S_TEXT/UTF8` is
`utf8`; TTML is `ttml`) and carry language and track name onto `lang` and
`label`. A format nothing maps to should degrade to skipping that one track
with a warning, never to failing the import, which is the behavior an `sbtl`
handler gets wrong today in the other direction.

Cue timing comes from the sample's own presentation time on the shared media
clock, which is what a text frame's timestamp means. Text is sparse, so a
subtitle track must not gate the import's progress on an open group waiting
for a cue that may be minutes away.

Cover a file with subtitles and no supported text format, a file with several
subtitle languages, and a file whose subtitle track is empty.

## Closes

- [#2280](https://github.com/moq-dev/moq/issues/2280) - close this issue when the quest finishes

## Related

- [MSF caption roles](/quest/m2/captions-msf.md) - the catalog half of the same
  gap
