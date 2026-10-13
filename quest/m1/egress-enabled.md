# [L] Native egress skips disabled renditions

## Goal

Every native consumer that picks or exports renditions from a hang catalog
skips a rendition marked `enabled: false`, as the hang draft requires ("a
consumer MUST NOT select the rendition"), and follows the catalog when a
rendition flips: a consumer serving a rendition that becomes disabled moves
to an enabled one, and one with nothing enabled serves nothing until a
rendition re-enables. This covers manual disable, simulcast rung disable, and
a failed rendition alike. No wire change.

## Plan

`@moq/watch` is the reference: video and audio selection drop disabled
renditions before ranking (`js/watch/src/video/source.ts`,
`js/watch/src/audio/source.ts`) and re-run whenever the catalog changes.

Already correct, from #5273:

- `moq play` (`rs/moq-cli/src/play/media.rs`) picks from `enabled()` and
  stops a rendition the catalog disables, then reselects.
- `moqsrc` (`rs/moq-gst/src/source/imp.rs`).
- moq-ffi and moq-c refuse to subscribe a disabled rendition; the binding's
  application owns the pick and sees the `enabled` field.
- moq-transcode prefers an enabled source and takes a disabled one only so
  its rungs inherit `enabled: false`; that subscription carries no frames.

Still serving disabled renditions:

- WHEP (`rs/moq-rtc/src/egress.rs`): `pick_audio` and `pick_video` take the
  first match from `ranked()`, which only sorts disabled entries last, so a
  lone or all-disabled rendition is still picked. The catalog is a snapshot
  from session start, so a flip is never seen.
- HLS and DASH export (`rs/moq-hls/src/export/renditions.rs`): `sync`
  registers every rendition, so the multivariant playlist and manifest list
  disabled ones.
- moq-mux exports, behind `moq subscribe`, RTMP play and dial, and SRT play:
  `select::Broadcast` keeps disabled renditions; the H.264 and H.265 Annex-B
  exports take the first matching rendition by name; single-track FLV uses
  `ranked()` and binds before its header; fMP4, MKV, TS, and multitrack FLV
  mux every selected rendition. RTMP's `play_selection` also refuses a
  multitrack play over a disabled rendition it can't carry.

Decided with the maintainer (2026-10-13):

- **One rule in moq-mux, no new hang API.** `hang` already has
  `Video::enabled()` and `Audio::enabled()`. Treat a disabled rendition as
  absent where moq-mux already drops renditions on hang's rules: beside
  `supported` in `select::Broadcast` and in each export's catalog intake
  (where `Source::retain_valid_media` runs), so no export carries its own
  check. Prefer extending an existing filter over adding a parallel one.
- **A muxed export puts only enabled renditions in its header.** After the
  header, a rendition that gets disabled keeps its declared track, idle
  until it re-enables, so MKV and fMP4 never end on a rung disable (a
  post-header removal is an error in MKV, and fMP4 refuses a track added
  after init). A rendition first enabled after the header follows the
  format's existing rule for added tracks.
- **HLS and DASH omit a disabled rendition from the multivariant playlist
  and manifest, but keep its window and media playlist serving.** Closing it
  would end the playlist with `EXT-X-ENDLIST`, which reads as a finished
  broadcast (see the [HLS export failure signal](/quest/m1/hls-failure-signal.md)),
  and a re-enabled rendition keeps its media sequence and history.
- **WHEP follows the catalog for the session's life** and re-picks each
  negotiated media line on a change, switching at the new track's first
  keyframe rather than leaving a gap. With no ABR on the peer, it serves the
  current best enabled rendition, so a re-enabled top rung wins back. The
  SDP codec list stays fixed for the session.
- **Related to simulcast rung disable, not Required.** The publisher side
  stands alone, and manual disable already has this gap, so rung disable
  stays ready.

Tests on mocked time with in-process broadcasts: a mixed catalog never
selects the disabled rendition; an all-disabled catalog selects nothing; a
disable mid-stream moves WHEP and the single-rendition exports to the
enabled sibling and a re-enable restores it; HLS drops and restores the
multivariant entry without resetting the media sequence; a muxed export
leaves a disabled rendition out of its header, and a rendition disabled
after the header idles its track without ending the export and resumes it
on re-enable.

Public API: none expected beyond `select::Broadcast`'s documented behavior.
Update the egress docs (`doc/bin/cli.md`, `rtc.md`, `hls.md`, `rtmp.md`,
`srt.md`) where they describe rendition selection.

## Related

- [Simulcast rung disable](/quest/m1/simulcast-rung-disable.md) - publishers disabling top rungs, which strands WHEP and HLS viewers until this lands
- [Rendition preference](/quest/m1/rendition-preference.md) - reworks WHEP's pick across negotiated codecs; land the two picks together or rebase one on the other
- [Viewer up-switch](/quest/m1/viewer-upswitch.md) - the viewer side of moving between renditions as capacity changes
