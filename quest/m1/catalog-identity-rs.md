# [L] Rust identity enforcement

## Goal

No Rust publisher redefines a track it has published. A change to a
rendition's identity, the fields a consumer cannot change without
reinitializing its decoder or demuxer (`codec`, `description`, `container`,
`display_aspect_width`/`display_aspect_height`, `sample_rate`,
`channel_count`), publishes a new rendition name, per the hang draft's
[Track Identity](/drafts/draft-lcurley-moq-hang.md#identity) section. Every
other field, including coded size, `bitrate`, and `framerate`, updates in
place. A replayed archive catalog then describes every group of the tracks it
lists.

## Plan

- ✅ Decided 2026-10-13 (maintainer, #5459): split from the catalog identity
  quest. The contract and docs landed there; `@moq/publish` enforces the
  browser side.
- ✅ Decided 2026-10-13 (maintainer, #5459): the Opus description is probed
  up front.
- ✅ Decided 2026-10-15 (maintainer): mint a new rendition name, not a new
  epoch; reverses the #5459 decision. Restarting the whole broadcast is an
  extreme response to one rendition changing, and a name always means the
  same content, so changed content gets a new name. `@moq/publish` keeps the
  catalog ID and points its `track` at the next free `<id>/<n>` (#5552); each
  Rust violator mints a name likewise, as FLV, HLS import, and transcode rungs
  already do. A new epoch stays reserved for a whole-broadcast restart.
- ✅ Decided 2026-10-15 (maintainer): only fields that force a decoder
  reinitialization mint a new track; everything else updates in place. New
  tracks have user impact, so they are minted only when they must be. This
  drops the #5459 coded-size ceilings and the rule that `bitrate`/`framerate`
  never fall: a decoder follows a resize in band, and `codec` names a level
  covering every picture, so outgrowing the size takes a new name only when
  the codec string changes.

Guard `moq-mux` `Rendition::set`, `replace`, and `stage`
(`rs/moq-mux/src/catalog/tracks.rs`): refuse a change to identity, and let
every other field update. Then fix each violator the 2026-10-13 audit found,
minting a name where the identity changes and updating in place otherwise:

- The h264/h265 importers (an inline SPS, or a new avcC/hvcC through
  `initialize`), av1 (a new sequence header, including the av1C defaults and
  the minimal placeholder), and vp8/vp9 (keyframe size; VP9's level follows
  it) rewrite in place, including MPEG-TS and `moq-rtc` ingest through them.
  A size change alone updates in place; a new description or codec string
  mints a name.
- A codec hint published up front is republished with the real
  profile/level by the first keyframe. Publish once from the stream, or mint
  when the codec string differs.
- `moq-video` reopening onto another camera mode or backend
  (`source_resize_updates_the_published_rendition` asserts the rewrite).
- `moq-c` re-declaring a config under the same name, and `moq-gst` reusing an
  explicit pad name after caps renegotiation.
- TS AAC/MP2/AC-3 and FLV legacy audio fix the config from the first header
  and silently ignore a later change; refuse or mint instead. A PMT that
  changes a PID's stream type keeps the old importer.
- `moq-audio`/`moq-ffi` `set_bitrate` should update the catalog's bitrate.

FLV, fMP4, MKV, HLS import, and `moq-transcode` rungs already comply; keep
their tests. Update the tests that assert in-place identity rewrites
(`tracks.rs`, `moq-video`, `moq-gst`, `moq-c`); a same-name resize, such as
the transcode fixture's, is now allowed.

Later, at lower priority: watch and HLS export consumer coverage for in-band
resolution changes, rendition switches, reconnects, and late joiners against
the contract.

## Related

- [Broadcast archives](/quest/m1/archive/README.md) - an archived catalog describes every stored group by the identity contract
