---
title: TypeScript
description: The browser implementation, published as @moq/* on npm
---

# TypeScript

A from-scratch implementation for browsers, built on WebTransport, WebCodecs,
and WebAudio. `@moq/net` also runs in Node, Bun, and Deno.

For a player or publisher, start with the [web components](#web-components).
For custom data, start with [@moq/net](/lib/js/net); for a meeting, use
[@moq/room](/lib/js/room). [Examples](/setup/examples) links complete apps and
small examples. The [API reference](/lib/js/api/) covers every package; source lives under
[`js/`](https://github.com/moq-dev/moq/tree/main/js).

## Packages

| Package | Does |
| --- | --- |
| [@moq/net](/lib/js/net) | The pub/sub layer: connections, broadcasts, tracks, groups, frames, discovery. |
| [@moq/pattern](/lib/js/api/pattern) | Exact path patterns: grammar, matching, and set algebra. Re-exported by `@moq/net` and `@moq/auth`. |
| [@moq/hang](/lib/js/hang) | The media layer: catalog types and containers. |
| [@moq/libav](/lib/js/api/libav) | libav.js decoding Opus and AAC, which `@moq/hang` loads as its WebCodecs fallback in Node, Bun, and browsers without `AudioDecoder`. LGPL-2.1-or-later. |
| [@moq/watch](/lib/js/watch) | Subscribe, decode, and render. `<moq-watch>` plus an optional UI overlay. |
| [@moq/video](/lib/js/video) | Draw video frames into a canvas through WebGPU or Canvas2D. Used by `@moq/watch` and `@moq/publish`. |
| [@moq/publish](/lib/js/publish) | Capture, encode, and publish. `<moq-publish>` plus an optional UI overlay. |
| [@moq/ui](/lib/js/api/ui) | Presentation components shared by the watch and publish overlays: control buttons, a stats graph, and the support banner. |
| [@moq/room](/lib/js/room) | Headless rooms: announce-derived roster, local publish, remote watch, and a chat track. |
| [@moq/auth](/lib/js/auth) | Validate a relay's request, build its grant, and mint and verify relay JWTs. |
| [@moq/clock](/lib/js/clock) | Logical monotonic time, wall time, and deterministic timers. |
| [@moq/signals](/lib/js/signals) | The reactive primitives every package exposes its state through. |
| [@moq/json](/lib/js/json) | JSON over tracks: snapshots with merge-patch deltas, or append logs. |
| [@moq/e2ee](https://github.com/moq-dev/moq/tree/main/js/e2ee) | End-to-end encryption of groups, datagrams, and track names, scoped to a publisher epoch. Workspace-only until the browser components use it. |
| [@moq/sign](https://github.com/moq-dev/moq/tree/main/js/sign) | Per-frame Ed25519 signatures, so relays forward content they cannot forge, retime, or relocate. Workspace-only until the mesh uses it. |
| [@moq/loc](/lib/js/api/loc), [@moq/msf](/lib/js/api/msf) | The IETF LOC container and MSF catalog. |
| [@moq/boy](/lib/js/api/boy) | The [MoQ Boy](/bin/demo) player element. |

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

Below the elements, `Watch.Player` owns a headless playback pipeline,
`Publish.Broadcast` owns a headless publisher, and `@moq/net` is the protocol itself. Examples:
[`js/net/examples/`](https://github.com/moq-dev/moq/tree/main/js/net/examples)
covers connecting, publishing, subscribing, and discovery.

## Browser support

| Browser | Transport |
| --- | --- |
| Chrome, Edge 97+ | WebTransport |
| Firefox 153+ | WebTransport. Earlier Firefox ships it but allows too few incoming streams, so the client falls back to WebSocket there. |
| WebKit (Safari, Chrome, Firefox, and Edge on iOS) | WebSocket fallback. Safari 26.4 ships WebTransport, but WebKit bugs stall long sessions, so the client doesn't use it yet. |
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
