# [L] Hang thumbnails

## Goal

A hang broadcast can carry still images in a new `thumbnail` catalog
section: renditions keyed by name, each with a mime (`image/png` for
lossless, and lossy formats such as `image/jpeg`), a width, and a height,
one image per group. Rust (`hang`) and `@moq/hang` both read and write it.
A moq-video producer scales and encodes the newest captured or decoded
frame into each rendition whenever its track
[refreshes](/quest/m2/refresh-interval.md), so a reader that arrives or asks
gets a current still, and the publisher encodes no more than one per
interval. Before any reader has asked there is no still: the first reader's
join asks for the first one at once, and the interval applies from then on.

## Plan

Planned 2026-11-13 (maintainer, /quest-plan of a remote desktop for
kixel.ai). The consumer is Kixel's desktop host: an agent's screenshot is a
lossless rendition, refreshed after each action settles, and the dashboard's
previews are a small lossy one.

Decided:

- ✅ A `thumbnail` section in the hang schema (rejected: the existing
  `binary` section with `mime: image/png`). Stills get typed fields (size,
  format), and several renditions, as video has.
- ✅ The schema and the moq-video producer in one quest (rejected: schema
  only). Rooms and the studio would otherwise each write the encoder again.
- ✅ m4 (rejected: m3). Additive, and the consumer is Kixel's later (m2)
  plans.

Facts found while planning:

- Both catalog parsers tolerate unknown sections (`z.looseObject` in
  `js/hang/src/catalog/root.ts`, and serde without `deny_unknown_fields`), so
  the section is additive on the wire too.
- `rs/hang/src/catalog/root.rs` already has a test with
  `"thumbnail": {"mode":"snapshot","mime":"image/jpeg"}` under `binary`.
  Replace it with the new section rather than keeping two ways to say the
  same thing.

Recommendations for the PR:

- A group's identity is its start timestamp, and the same identity must never
  carry different content. So each still is stamped with the time it was
  sampled, on the root's `media.clock`, strictly after the previous still's
  (one tick later when two samplings collide), and never with the source
  frame's own timestamp: a static screen would give two refreshes the same
  frame and so the same start. The still shows the newest frame at that
  time, so it lines up with the video at its timestamp.
- Each still is encoded after the request that asks for it, from the newest
  frame the capture has delivered by then, and never reuses an earlier
  encode, so it meets the `refresh()` request barrier. A capture that
  delivers frames only on change (PipeWire does) has its last frame as the
  current screen, so the producer must not wait for a newer one. A caller
  that needs the result of an action calls `refresh()` once the action has
  settled.
- The producer refuses, at setup, a mime it cannot encode, rather than
  publishing a rendition it never fills. PNG and JPEG first. Take a
  dependency for encoding only at its newest stable version, and only if it
  earns its build time.
- Readers, such as a watch poster image or a room's grid, are follow-ups
  once someone needs them.

Public API: additive. Wire: the hang catalog gains an optional section.

## Required

- [Refresh a track by interval, and on request](/quest/m2/refresh-interval.md) - what makes a one-image group refresh on demand
