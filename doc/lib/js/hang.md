---
title: "@moq/hang"
description: The media layer in TypeScript
---

# @moq/hang

[![npm](https://img.shields.io/npm/v/@moq/hang)](https://www.npmjs.com/package/@moq/hang)

The [hang media format](/concept/hang) as TypeScript types and codecs, shared
by [`@moq/watch`](/lib/js/watch) and [`@moq/publish`](/lib/js/publish).

- **Catalog**: zod schemas for the root, video and audio renditions, containers, and timelines. The root is a loose object, so `z.extend(Catalog.RootSchema, { yourSection })` adds your own.
- **Containers**: `Container.Legacy` producer/consumer and `Container.Cmaf` init and data segment helpers.
- **Utilities**: hex, priority and latency math, an Opus polyfill for browsers without a native decoder, and the browser quirks the media packages work around.

```ts
import * as Catalog from "@moq/hang/catalog";
import * as Container from "@moq/hang/container";
```

Most apps never import it directly; the elements and `Broadcast` classes in
the watch and publish packages do. Reach for it when hand-rolling a catalog
or building a custom player.
