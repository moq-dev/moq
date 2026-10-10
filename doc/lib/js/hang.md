---
title: "@moq/hang"
description: The media layer in TypeScript
---

# @moq/hang

[![npm](https://img.shields.io/npm/v/@moq/hang)](https://www.npmjs.com/package/@moq/hang)

The [hang media format](/concept/hang) as TypeScript types and codecs, shared
by [`@moq/watch`](/lib/js/watch) and [`@moq/publish`](/lib/js/publish).

- **Catalog**: zod schemas for the root, renditions, data tracks, and `archive`. The root is a loose object, so `z.extend(Catalog.RootSchema, { yourSection })` adds your own. `Catalog.watch(broadcast)` iterates validated catalog updates, and `Catalog.ranked(renditions)` orders video renditions best first, the order `<moq-watch>` picks from.
- **Containers**: `Container.Legacy` producer/consumer and `Container.Cmaf` init and data segment helpers.
- **Utilities**: priority and latency math, an Opus polyfill for browsers without a native decoder, and the browser quirks the media packages work around.

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
