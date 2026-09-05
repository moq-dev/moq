---
title: TypeScript
description: The browser implementation, published as @moq/* on npm
---

# TypeScript

A from-scratch implementation for browsers, built on WebTransport, WebCodecs,
and WebAudio. `@moq/net` also runs in Node, Bun, and Deno.

## Packages

| Package | Does |
| --- | --- |
| [@moq/net](/lib/js/net) | The pub/sub layer: connections, broadcasts, tracks, groups, frames, discovery. |
| [@moq/hang](/lib/js/hang) | The media layer: catalog types and containers. |
| [@moq/watch](/lib/js/watch) | Subscribe, decode, and render. `<moq-watch>` plus an optional UI overlay. |
| [@moq/publish](/lib/js/publish) | Capture, encode, and publish. `<moq-publish>` plus an optional UI overlay. |
| [@moq/token](/lib/js/token) | Mint and verify relay JWTs. |
| [@moq/signals](/lib/js/signals) | The reactive primitives every package exposes its state through. |
| [@moq/json](https://www.npmjs.com/package/@moq/json) | JSON over tracks: snapshots with merge-patch deltas, or append logs. |
| [@moq/flate](https://www.npmjs.com/package/@moq/flate) | Group-scoped DEFLATE for any track. |
| [@moq/loc](https://www.npmjs.com/package/@moq/loc), [@moq/msf](https://www.npmjs.com/package/@moq/msf) | The IETF LOC container and MSF catalog. |
| [@moq/boy](https://www.npmjs.com/package/@moq/boy) | The [MoQ Boy](/bin/demo) player element. |

## Web components

The fastest way in. No framework, no build step required:

```html
<script type="module">
    import "https://esm.sh/@moq/watch/element";
    import "https://esm.sh/@moq/publish/element";
</script>

<moq-publish url="https://relay.example.com/anon" name="room/alice.hang" source="camera">
    <video muted autoplay></video>
</moq-publish>

<moq-watch url="https://relay.example.com/anon" name="room/alice.hang">
    <canvas></canvas>
</moq-watch>
```

With a bundler, `bun add @moq/watch @moq/publish` and import the same
`/element` entrypoints (the suffix keeps tree-shaking from dropping the
registration). Add `/ui` for the ready-made control overlays. Every attribute
is also a typed, reactive JS property, and the elements expose their internal
pipeline (`broadcast`, `video`, `audio`, `signals`) for apps that want more.
They work in React, Vue, Solid, and plain HTML alike; `@moq/signals` ships
React and Solid adapters for the reactive state.

## JavaScript API

Below the elements, `Watch.Broadcast` and `Publish.Broadcast` are the same
pipelines without DOM, and `@moq/net` is the protocol itself. Examples:
[`js/net/examples/`](https://github.com/moq-dev/moq/tree/main/js/net/examples)
covers connecting, publishing, subscribing, and discovery.

## Browser support

| Browser | Transport |
| --- | --- |
| Chrome, Edge 97+ | WebTransport |
| Firefox 153+ | WebTransport. Earlier Firefox ships it but allows too few incoming streams, so the client falls back to WebSocket there. |
| Safari | WebSocket fallback. Safari 26.4 ships WebTransport, but WebKit bugs stall long sessions, so the client doesn't use it yet. |
| Anything else | Automatic WebSocket fallback, with TCP's head-of-line blocking |

WebCodecs support varies per codec and browser; `<moq-watch-support>` and
`<moq-publish-support>` render what the current browser can do. Outside
localhost the relay needs a real certificate, or the page must pin its
fingerprint.

## Server-side

`@moq/net` runs in Bun, Node 21+, and Deno over the WebSocket fallback with no
changes (older Node needs the `ws` polyfill on `globalThis`). For real QUIC on
the server, install the native
[`@moq/web-transport`](https://www.npmjs.com/package/@moq/web-transport)
polyfill. `@moq/hang`, `@moq/watch`, and `@moq/publish` are browser-only, so
server-side media work means raw tracks plus your own encoder.
