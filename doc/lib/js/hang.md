---
title: "@moq/hang"
description: The media layer in TypeScript
---

# @moq/hang

[![npm](https://img.shields.io/npm/v/@moq/hang)](https://www.npmjs.com/package/@moq/hang)

The [hang media format](/concept/hang) as TypeScript types and codecs, shared
by [`@moq/watch`](/lib/js/watch) and [`@moq/publish`](/lib/js/publish).

- **Catalog**: zod schemas for the root, renditions, data tracks, and `archive`. The root is a loose object, so `z.extend(Catalog.RootSchema, { yourSection })` adds your own. `Catalog.watch(broadcast)` iterates validated catalog updates, and parsing throws `Catalog.UnsupportedVersion` on a catalog that needs a newer moq-hang draft. `Catalog.Audio.ranked(renditions)` and `Catalog.Video.ranked(renditions)` order renditions best first, with enabled renditions ahead; `<moq-watch>` uses the video order.
- **Containers**: `Container.Legacy` producer/consumer and separate `@moq/hang/cmaf` decoding. Import the CMAF module only when reading fMP4.
- **Utilities**: priority and latency math, a libav polyfill (Opus, plus AAC decoding) for runtimes without native WebCodecs audio, and the browser quirks the media packages work around.

```ts
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
```

Most apps never import it directly; the elements and `Broadcast` classes in
the watch and publish packages do. Reach for it when hand-rolling a catalog
or building a custom player.

## Wall clock

A catalog root may carry a `clock` that maps PTS zero to wall time.
`Catalog.wallClockTime(clock, pts, ptsTimescale)` turns a frame's timestamp
into a `Date`. A `Container.Legacy` timestamp is microseconds, so pass
`1_000_000`.

```ts
for await (const root of Catalog.watch(broadcast)) {
	if (!root.clock) continue; // the publisher exposes no clock
	// `frame` comes from your own `Container.Legacy` consumer.
	const captured = Catalog.wallClockTime(root.clock, frame.timestamp, 1_000_000);
}
```

The mapping is fixed for the life of the broadcast and does not need an
`archive`. It says where PTS zero was on the publisher's clock, never whether
the viewer's clock agrees. Playback never reads it: `<moq-watch>` stays
arrival-based, so lining viewers up on wall time is the application's job and
needs clocks it already knows are synchronized.
