# [L] CEA-608/708 extraction

## Goal

Captions carried inside video SEI become a real text rendition at import. A
large share of existing broadcast content carries captions only this way, so
without extraction those broadcasts have no captions in MoQ at all.

## Plan

Parse the `user_data_registered_itu_t_t35` SEI payloads that carry CEA-708
(with 608 compatibility bytes inside) out of H.264 and H.265 access units at
import, and publish the decoded cues as a `text` rendition beside the video.

The fiddly part is that 608/708 is a stateful terminal protocol, not a cue
list. Pop-on, roll-up, and paint-on modes each build the visible caption
differently, and the decoder has to track the display buffer to know when a
cue starts and ends. 708 adds a service layer, so a stream can carry several
services (commonly a primary language and a secondary one), each of which
should become its own rendition with its own `lang`.

Emit cues on the shared media clock, taken from the access unit the SEI rode
in, so the text rendition needs no timeline of its own to stay in sync.

Parse at import rather than waiting on the SEI sidecar work, so captions do not
inherit that line's schedule. Once SEI is a sidecar the parser can read it from
there instead of walking the access unit, which is a refactor rather than a
prerequisite.

Whichever lands first, the import order is a contract, not an accident: caption
extraction sees the SEI before stripping removes it. Running the split first
silently produces a broadcast with no captions, which is the failure this quest
exists to prevent, so cover the ordering with a test rather than a comment.

## Related

- [SEI](/quest/m2/sei/README.md) - carries SEI byte-faithfully as a sidecar; if
  that line lands, this parser is a candidate to move onto it rather than
  walking the access unit itself
