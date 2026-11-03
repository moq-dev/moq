# [M] A republish announces as soon as its tracks are known

## Goal

After a TS import republishes on a clock step, the new instance's catalog,
and so its announcement, goes out as soon as every elementary stream's
config is known, with no extra wait. A stitched export then joins the new
instance's first group while it is still the newest, so a short-GOP feed
loses only the truncated picture at the cut, not a whole GOP.

## Plan

Decided 2026-11-03 (user, /quest-import of
[moq-dev/moq#7744](https://github.com/moq-dev/moq/issues/7744)).

- ✅ Remove the import-side wait beyond the last stream's config. Not chosen:
  announce at once with the predecessor's track list (the catalog is wrong
  when the switch changed a codec config, and a relay could query tracks
  the instance cannot serve yet). Not chosen: start a stitched replacement at
  its first group (output would stay behind `--delay` by the announce lag for
  the rest of the stream, against #7049). Not chosen: won't fix.
- Milestone: ✅ m1, in this epic. Not chosen: m2.
- Accepted: a feed whose stream configs arrive after its second keyframe
  still loses its first group. That cost comes from the source.

Facts at 976fece173: `Pending::announce` (`rs/moq-cli/src/publish.rs:359`)
holds the announcement until the catalog's first snapshot. The catalog
withholds that snapshot while any rendition's reservation is alive
(`catalog/producer.rs:257`). A republish reuses its predecessor's
`muxRate` (`publish.rs:946`), so the mux-rate reservation is already skipped.
The reporter measured every stream's first unit within 0.76 s of the cut,
but the announcement only at 1.2 s, and did not find which track resolves
last.

- Reproduce with a synthetic clock step on a 1.2 s GOP with several streams
  (H.264, MP2, AC-3, teletext, SCTE-35), real-time paced. Trace which
  reservation resolves last (`initial_reservation`, a rendition config, or
  decode-order admission holding frames) and remove the wait at its source.
- Test end to end: the stitched export of that switch starts at the new
  instance's first group and loses at most the truncated picture.
- Fix the `Pending` doc comment, which says "at least one servable track"
  while the code waits for every track.

Public API: none expected. Wire: none.

## Closes

- [#7744](https://github.com/moq-dev/moq/issues/7744) - a republish is announced 1.2 s late and a stitched export loses a GOP
