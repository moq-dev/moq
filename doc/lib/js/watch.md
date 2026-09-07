---
title: "@moq/watch"
description: Subscribe, decode, and render broadcasts in the browser
---

# @moq/watch

[![npm](https://img.shields.io/npm/v/@moq/watch)](https://www.npmjs.com/package/@moq/watch)

A player: subscribes to a broadcast, picks renditions, decodes with
WebCodecs, renders video to a canvas and audio through WebAudio, and keeps them
in sync at the latency you ask for.

```html
<script type="module">
    import "@moq/watch/element";
    import "@moq/watch/ui";       // optional overlay
</script>

<moq-watch-ui>
    <moq-watch url="https://relay.example.com/anon" name="room/alice.hang">
        <canvas></canvas>
    </moq-watch>
</moq-watch-ui>
```

## Attributes

| Attribute | |
| --- | --- |
| `url`, `name` | Relay URL (with `?jwt=` if needed) and broadcast name. |
| `paused`, `muted`, `volume` | The usual player controls, mirrored as reactive properties. |
| `latency` | Target latency: `"real-time"` (derived from RTT, the default), a number of ms, or `"instant"` to paint frames as they decode with no pacing at all. |
| `latency-min`, `latency-max` | Open a range instead: buffer freely between the floor and the ceiling and only skip ahead past the ceiling. |
| `jitter` | The jitter buffer in ms. |
| `visible` | Only subscribe to video while the element is on screen: a margin (`"20%"` default, `"200px"`), `"always"`, or `"never"`. |
| `reload` | Wait for the broadcast to be announced before subscribing (default on), so a player can be mounted before the stream exists. |
| `catalog-format` | `hang` (default, from the `.hang` suffix), `hangz` (compressed), `msf`, or `manual` to supply the catalog yourself. |

The overlay adds play/pause, volume, fullscreen, a quality selector, a
buffering indicator, an unsupported-codec warning, and a stats panel.
`<moq-watch-support>` shows what the browser can play.

## Binding from a framework

`import "@moq/watch/element"` registers `<moq-watch>` while the module
evaluates, so a browser-only entrypoint that imports it before mounting gets an
upgraded element. `el.broadcast`, `el.video`, `el.audio`, `el.sync`, and
`el.signals` are assigned by the constructor and readable right away.

Defer the import and that guarantee goes with it. A dynamic `import()` inside
`onMount`, one behind a `browser` guard, or a `<script>` that loads after the
markup all leave the tag unregistered until they land, and an element of an
unregistered tag is a plain `HTMLElement`. A framework binding
(Svelte's `bind:this`, React's `ref`) hands you that un-upgraded node, where
every property reads `undefined`:

```ts
// TypeError: el.broadcast is undefined
el.broadcast.out.catalog.subscribe(handler);
```

The browser upgrades the same node once the definition arrives, applying the
attributes it already has. Use a static import in browser-only entrypoints.
For SSR applications, run the following in a client-side mount hook, after
the node exists. The element module needs browser globals and must not be
imported during server rendering. Start the import before waiting for registration:

```ts
await import("@moq/watch/element");
await customElements.whenDefined("moq-watch");
el.broadcast.out.catalog.subscribe(handler);
```

## Custom tracks

The catalog schema is loose: sections `@moq/hang` doesn't recognize are passed
through to `broadcast.out.catalog`, a read-only signal you react to like any
other. Subscribe to the track it names off `broadcast.out.active`, the live
broadcast consumer, and decode JSON with
[`@moq/json`](https://www.npmjs.com/package/@moq/json).

```ts
import * as Json from "@moq/json";
import { Hang } from "@moq/watch";

// Run after the element module has loaded and the node has mounted.
const el = document.querySelector("moq-watch");
if (!el) throw new Error("Missing <moq-watch> element");

const dispose = el.signals.run((effect) => {
    const catalog = effect.get(el.broadcast.out.catalog) as { metadata?: unknown } | undefined;
    const active = effect.get(el.broadcast.out.active);

    const metadata = catalog?.metadata;
    if (metadata !== undefined &&
        (!Array.isArray(metadata) || !metadata.every((name): name is string => typeof name === "string"))) {
        throw new Error("Expected metadata to be an array of track names");
    }
    const name = metadata?.[0];
    if (!active || !name) return;

    const track = active.track(name).subscribe({ priority: Hang.Catalog.PRIORITY.catalog });
    effect.cleanup(() => track.close());

    const consumer = new Json.Snapshot.Consumer<unknown>(track);
    effect.spawn(async () => {
        for (;;) {
            const value = await Promise.race([effect.cancel, consumer.next()]);
            if (value === undefined) break;
            console.log("metadata", value);
        }
    });
});
```

Call `dispose()` from your framework's unmount cleanup when this subscription
is no longer needed. Removing the element disables playback but keeps its
effects open so the same node can reconnect.

The effect re-runs whenever the catalog or the active broadcast changes, so a
reconnect resubscribes on its own. A publisher that rewrites its catalog often
(a live encoder tweak) re-runs it too; memoize the track name with
`effect.computed` when that matters, as the
[watch demo](https://github.com/moq-dev/moq/blob/main/demo/web/src/index.ts)
does.

`el.catalog` is the same value read once, without subscribing. Reach the rest
of the pipeline through `el.broadcast`, `el.video`, `el.audio`, and
`el.signals`.

## Without the element

```ts
import * as Watch from "@moq/watch";

const broadcast = new Watch.Broadcast({ connection, enabled: true, name: "alice.hang" });
```

`Watch.Broadcast`, `Video.Decoder`, `Video.Renderer`, `Audio.Decoder`, and
`Audio.Emitter` are the pieces the element assembles; every input and output
is a signal from [`@moq/signals`](/lib/js/signals). Load from a CDN
(`https://esm.sh/@moq/watch/element`) for a no-build embed.

## Buffered playback

By default the player minimizes latency: it skips ahead whenever the buffer
grows past the target. Content produced faster than real time, such as a TTS
response emitted in one burst with future timestamps, wants the opposite. Set
`latency-max` above `latency-min` to play through at the encoded pace:

```html
<moq-watch url="..." name="bot/tts.hang" latency-min="100" latency-max="30000"></moq-watch>
```

Only the floor is held as decoded PCM; the rest stays as encoded frames with
backpressure on the decoder, so a large ceiling is cheap. `el.reset()`
flushes and re-anchors at the next frame, which is how a producer interrupts
an utterance.
