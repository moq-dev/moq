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
| `paused`, `muted`, `volume` | The usual player controls. |
| `delay` | How far playback trails the live edge: `"auto"` (the default, sized from how late frames arrive; see [audio jitter](/concept/audio-jitter)), a duration like `"300ms"`, or `"instant"` to paint video as it decodes, with no audio. |
| `buffer` | How far ahead of the live edge media may run; see [buffered playback](#buffered-playback). |
| `captions` | The caption track to show. |
| `visible` | Only download video while the element is on screen (or near it). |
| `announced` | Wait for the broadcast to be announced before subscribing (default on), so a player can mount before the stream exists. |
| `catalog-format` | `hang`, `hangz`, `msf`, or `manual`; detected from the name by default. |

Every attribute is also a reactive property. The
[README](https://www.npmjs.com/package/@moq/watch) lists types and defaults.
The overlay adds play/pause, volume, fullscreen, a quality selector, a
buffering indicator, and a stats panel.

`el.broadcast.out.status` is `offline`, `loading`, `live`, or `error`. On
`error` the origin refused the broadcast, `el.broadcast.out.error` says why,
and the player does not ask again until it is pointed elsewhere or re-enabled.

## Binding from a framework

`import "@moq/watch/element"` registers `<moq-watch>` when the module
evaluates. Until then the tag is a plain `HTMLElement`, so a framework ref
(Svelte's `bind:this`, React's `ref`) to it reads `undefined` for
`el.broadcast` and friends. Import the element statically in browser-only
entrypoints. With SSR, import it in a client-side mount hook and wait:

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

    const consumer = new Json.Snapshot.Consumer<unknown>({ track });
    effect.spawn(async () => {
        for (;;) {
            const value = await effect.race(consumer.next());
            if (value === undefined) break;
            console.log("metadata", value);
        }
    });
});
```

Call `dispose()` on unmount. The effect re-runs when the catalog or the
active broadcast changes, so a reconnect resubscribes on its own.

## Without the element

```ts
import * as Moq from "@moq/net";
import * as Watch from "@moq/watch";

// Shared with every other component pointed at the same relay; the broadcast
// handle reads from its origin and spans reconnects.
const connection = new Moq.Connection({ url: new URL("https://relay.example.com/anon") });
const player = new Watch.Player({
    origin: connection.origin,
    probe: connection.probe,
    name: Moq.Path.from("alice.hang"),
    canvas,
});
// player.broadcast, player.video, player.audio, player.text, player.sync,
// player.renderer, and player.emitter expose the pipeline.
// Call player.close() when playback ends.
```

Pass a signal from [`@moq/signals`](/lib/js/signals) for any control you want
to change later, such as `muted` or `delay`. `Player` is the pipeline inside
`<moq-watch>`; its parts are exported for custom composition.

## Buffered playback

By default the player minimizes latency: it skips ahead whenever media piles
up past the delay. Content produced faster than real time, such as a TTS
response emitted in one burst with future timestamps, wants the opposite. Set
`buffer` to how far ahead it may run and it plays through at the encoded pace:

```html
<moq-watch url="..." name="bot/tts.hang" delay="100ms" buffer="30s"></moq-watch>
```

Durations need a unit; a bare number is rejected. Audio holds the buffer as
encoded frames, so a large one is cheap; video waits as decoded pictures, which
hold decoder memory, so a long video buffer is not. `el.reset()` flushes and re-anchors at the
next frame, which is how a producer interrupts an utterance.

## Strict CSP

The audio worklet loads from a `blob:` URL by default, so a CSP must allow
`blob:` in `script-src`. Otherwise, copy `node_modules/@moq/watch/assets/*`
into a directory your origin serves and point the package at it before
playback starts:

```ts
import * as Watch from "@moq/watch";

Watch.assets("/moq/");
```

The URL must end with `/`. Copy the files again on every upgrade: the worklet
changes with the package. `@moq/room` and `@moq/boy` play through
`@moq/watch`, so this one call covers them.
