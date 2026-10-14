# [L] Rust identity enforcement

## Goal

No Rust publisher redefines a track it has published. A change to a
rendition's identity (`codec`, `description`, `container`, `sample_rate`,
`channel_count`) or growth past its `coded_width`/`coded_height` ceiling
publishes a new rendition name or a new broadcast epoch, per the hang draft's
[Track Identity](/drafts/draft-lcurley-moq-hang.md#identity) section, and
`bitrate`/`framerate` never fall. A replayed archive catalog then describes
every group of the tracks it lists.

## Plan

- ✅ Decided 2026-10-13 (maintainer, #5459): split from the catalog identity
  quest. The contract, docs, and the `js/publish` ceiling landed there;
  [JS identity enforcement](/quest/m1/catalog-identity-js.md) owns the
  browser side.
- ✅ Decided 2026-10-13 (maintainer, #5459): JS enforces identity by
  republishing under a new epoch with the Opus description probed up front,
  and the `js/publish` video ceiling keeps growing per side.

Open: whether each Rust violator mints a new rendition name (as FLV, HLS
import, and transcode rungs already do) or restarts under a new epoch (as JS
will). Recommended: mint a name where the catalog can keep both, and take a
new epoch only for a whole-broadcast restart.

Guard `moq-mux` `Rendition::set`, `replace`, and `stage`
(`rs/moq-mux/src/catalog/tracks.rs`): refuse a change to identity or a set
ceiling, and keep `resolved`'s `bitrate`/`framerate` from falling. Then fix
each violator the 2026-10-13 audit found:

- The h264/h265 importers (an inline SPS, or a new avcC/hvcC through
  `initialize`), av1 (a new sequence header, including the av1C defaults and
  the minimal placeholder), and vp8/vp9 (keyframe size; VP9's level follows
  it) rewrite in place, including MPEG-TS and `moq-rtc` ingest through them.
  Below the ceiling, resolution changes in band with no catalog update.
- A codec hint published up front is republished with the real
  profile/level and size by the first keyframe. Publish once from the stream,
  or treat the hint as the ceiling.
- `moq-video` reopening onto another camera mode or backend
  (`source_resize_updates_the_published_rendition` asserts the rewrite).
- `moq-c` re-declaring a config under the same name, and `moq-gst` reusing an
  explicit pad name after caps renegotiation.
- TS AAC/MP2/AC-3 and FLV legacy audio fix the config from the first header
  and silently ignore a later change; refuse or mint instead. A PMT that
  changes a PID's stream type keeps the old importer.
- `moq-audio`/`moq-ffi` `set_bitrate` raising past the advertised bitrate
  should raise the catalog.

FLV, fMP4, MKV, HLS import, and `moq-transcode` rungs already comply; keep
their tests. Update the tests that assert in-place rewrites (`tracks.rs`,
`moq-video`, `moq-gst`, `moq-c`, and the transcode fixture's same-name
resize).

Later, at lower priority: watch and HLS export consumer coverage for in-band
resolution changes, rendition switches, reconnects, and late joiners against
the contract. Watch's `renditionJitter` fallback
(`js/watch/src/video/playhead.ts`) reads `framerate` as the live cadence, but it
is now a maximum, so a stream running below it gets less than a frame interval
of buffer when no `jitter` is advertised.

## Related

- [JS identity enforcement](/quest/m1/catalog-identity-js.md) - the same contract for `@moq/publish`
- [Archive](/quest/m1/archive/README.md) - storage and replay consume the identity contract
- [Broadcast epochs](/quest/m0/broadcast-epoch/README.md) - a restart is a new epoch rather than a changed track
