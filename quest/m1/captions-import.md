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
`vtt` to preserve its explicit end time; TTML is `ttml`) and carry language and track name onto `lang` and
`label`. A format nothing maps to should degrade to skipping that one track
with a warning, never to failing the import, which is the behavior an `sbtl`
handler gets wrong today in the other direction.

Cue timing comes from the sample's own presentation time on the shared media
clock, which is what a text frame's timestamp means. Text is sparse, so a
subtitle track must not gate the import's progress on an open group waiting
for a cue that may be minutes away.

Cover a file with subtitles and no supported text format, a file with several
subtitle languages, and a file whose subtitle track is empty.

### Draft progress and remaining scope

The draft imports MKV S_TEXT/UTF8 and S_TEXT/WEBVTT, and fMP4 wvtt,
as sparse WebVTT text tracks. It preserves source language and label, explicit
cue duration, identifiers and settings, and closes each text group immediately.
Matroska WebVTT retains its codec header and comments, and converts relative
inline timestamps back to the shared media clock. Unknown subtitle encodings
skip only their track, as required by this quest.

The original raw UTF-8 mapping discarded SRT end times: Hang utf8 is roll-up
text shown until the next cue. Use VTT for timed SRT so gaps and overlaps remain
representable. A regression for a 250 ms cue ending at 1.5 s failed before this
change and passes afterward. S_TEXT/TTML is not a registered Matroska mapping,
so the draft no longer advertises it.

TTML import remains open. MP4 stpp identifies an XML namespace, not necessarily
TTML, and copying XML is insufficient when the importer shifts the source clock.
Choose whether to support a bounded TTML/IMSC timing profile with explicit
namespace validation and complete timestamp conversion, or retain unsupported
XML tracks as skipped. Recommend the supported-profile implementation before
claiming TTML support; preserve this quest and keep the PR draft until that
scope and its tests are complete. Do not relabel arbitrary XML as TTML.

## Related

- [MSF caption roles](/quest/m1/captions-msf.md) - the catalog half of the same
  gap
