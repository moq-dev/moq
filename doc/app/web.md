---
title: Web Demo
description: Browser-based demo application for MoQ
---

# Web Demo

The web demo application showcases MoQ's browser capabilities, allowing you to publish and watch live streams directly from a web browser.

## Overview

Located at `js/demo/`, the web demo provides:

- **Publishing** — Capture camera, microphone, or screen and publish via MoQ
- **Watching** — Subscribe to and render live broadcasts with low latency
- **Web Components** — Uses `<moq-publish>` and `<moq-watch>` custom elements

## Running Locally

```bash
cd js/demo
bun install
bun run dev
```

Then open [http://localhost:5173](http://localhost:5173) in a browser with WebTransport support (Chrome 97+, Edge 97+).

## Related

- [@moq/watch](/js/@moq/watch) — Watch/subscribe package
- [@moq/publish](/js/@moq/publish) — Publish package
- [Relay setup](/app/relay/) — Server to connect to
