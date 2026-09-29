---
title: "@moq/hang"
description: The media layer in TypeScript
---

# @moq/hang

[![npm](https://img.shields.io/npm/v/@moq/hang)](https://www.npmjs.com/package/@moq/hang)

The [hang media format](/concept/hang) as TypeScript types and codecs, shared
by [`@moq/watch`](/lib/js/watch) and [`@moq/publish`](/lib/js/publish).

- **Catalog**: zod schemas for the root, video, audio and text renditions, the JSON and binary data track sections, containers, and the `archive` entry (timeline track plus optional replay, store, and version). The root is a loose object, so `z.extend(Catalog.RootSchema, { yourSection })` adds your own.
- **Containers**: `Container.Legacy` producer/consumer and `Container.Cmaf` init and data segment helpers.
- **Utilities**: hex, priority and latency math, an Opus polyfill for browsers without a native decoder, and the browser quirks the media packages work around.

```ts
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
```

`Catalog.watch(broadcast)` iterates validated catalog roots. It throws
`Catalog.TooManyRenditions` for an update above the 64 rendition limit, and
`Catalog.EscapingBroadcast` for a `broadcast` reference that walks above the
handle's `path`. Run the same checks on a catalog from another source with
`Catalog.checkRenditions(root)` and `Catalog.checkResolvable(root, base)`.
`Hang.Timeline.Consumer.subscribe(broadcast, root.archive)` reads segment
`push`, `pop`, and `skip` events when a root advertises an archive.

Most apps never import it directly; the elements and `Broadcast` classes in
the watch and publish packages do. Reach for it when hand-rolling a catalog
or building a custom player.

## Wall clock

A catalog root may carry a `clock`: `{ wall, timescale }`, the wall-clock time
of PTS zero in `timescale` units (microseconds by default) since 2020-01-01.
`Catalog.wallClockTime(clock, pts, ptsTimescale)` maps a frame's PTS to a
`Date`. A `Container.Legacy` frame timestamp is microseconds, so pass `1_000_000`.

```ts
for await (const root of Catalog.watch(broadcast)) {
	if (!root.clock) continue; // the publisher exposes no clock
	// `frame` comes from your own `Container.Legacy` consumer.
	const captured = Catalog.wallClockTime(root.clock, frame.timestamp, 1_000_000);
}
```

- The mapping is fixed for the life of the broadcast. A discontinuity or a
  system-clock adjustment on the publisher does not change it.
- It is independent of `archive`. A live-only broadcast carries a `clock` with
  no segment index, so a DVR view can label its timeline from it either way.
- A `Date` holds whole milliseconds, so anything finer in the clock is
  truncated.
- `wallClockTime` throws on an invalid input or a clock total past the
  safe-integer range, but a result past the `Date` range comes back as an
  invalid `Date` rather than throwing.

Two machines' wall times are only comparable when your application already
knows their clocks are synchronized. The catalog says where PTS zero was on the
publisher's clock, never whether the viewer's clock agrees with it. If you do
know, set `delay` to `target - (arrived - captured)`, where `arrived` is the
viewer's `Date.now()` for the frame that arrived earliest relative to its
timestamp, since `<moq-watch>` anchors playback on that frame. That holds only
with the default zero `buffer`; a lookahead moves the anchor. `<moq-watch>` adds
the largest active rendition's buffer on top, so a frame renders at about
`captured + target` plus that buffer: the rendition's catalog `delay` plus its
`jitter` (one video frame or one codec frame when unset), and for audio the
worklet quantum too. Viewers only line up when that buffer matches, and a
negative result means the target is already missed.

The library never does this for you. Playback stays arrival-based: it does not
read the `clock`, estimate the viewer's clock from the session RTT, or
exchange time over a track, and a timestamp stays relative to the broadcast
rather than a reading of any wall clock.
