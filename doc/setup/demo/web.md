---
title: Web Demo
description: Browser-based watch and publish demo for MoQ.
---

# Web Demo

A browser-based demo for watching and publishing live streams via MoQ. Uses the `<moq-watch>` and `<moq-publish>` web components.

## Running

```bash
just web
```

This starts three components in parallel:

1. **Relay** — a localhost MoQ relay server
2. **Publisher** — `moq-cli` publishing Big Buck Bunny via ffmpeg
3. **Web server** — a Vite dev server with the demo UI

Once running, open the browser to watch the stream, publish from your camera, or both.

## What It Shows

- **Watching** — subscribe to live broadcasts with low latency
- **Publishing** — capture camera, microphone, or screen and publish via MoQ
- **Web Components** — `<moq-watch>` and `<moq-publish>` custom elements
- **Discovery** — auto-discover available broadcasts via announcements

## Source Code

- **Web app**: [`demo/web/src/`](https://github.com/moq-dev/moq/tree/main/demo/web/src/)
- **Justfile**: [`demo/web/justfile`](https://github.com/moq-dev/moq/tree/main/demo/web/justfile)

## Related

- [@moq/watch](/js/@moq/watch) — Watch/subscribe package
- [@moq/publish](/js/@moq/publish) — Publish package
- [Relay setup](/app/relay/) — Server configuration
