---
title: "@moq/watch"
description: Subscribe, decode, and render broadcasts in the browser
---

# @moq/watch

[![npm](https://img.shields.io/npm/v/@moq/watch)](https://www.npmjs.com/package/@moq/watch)
[![API](https://img.shields.io/badge/API-reference-blue)](/lib/js/api/watch)

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
| `versions` | Protocol versions to offer, most preferred first, comma-separated like `moq-transport-18,moq-lite-06`. Every stable version by default. |
| `paused`, `muted`, `volume` | The usual player controls. |
| `delay` | How far playback trails the live edge: `"auto"` (the default, sized from how late frames arrive; see [audio jitter](/concept/audio-jitter)), a duration like `"300ms"`, or `"instant"` to paint video as it decodes, with no audio. |
| `buffer` | How far ahead of the live edge media may run; see [buffered playback](#buffered-playback). |
| `captions` | The caption track to show. |
| `visible` | Only download video while the element is on screen (or near it). |
| `backend` | How the canvas is drawn; see [@moq/video](/lib/js/video). `auto` (default), `webgpu`, or `2d`. |
| `follow` | Follow the broadcast's announcements (default on): wait for one before subscribing, so a player can mount before the stream exists, and move to each restart. Off requests the name once. |
| `catalog-format` | `hang`, `msf`, or `manual`; detected from the name by default. |

Every attribute is also a reactive property, and property or UI changes reflect
back into the attributes. An invalid value is refused rather than defaulted: the
property setter throws, and invalid markup reports a browser `error` event and
suspends the element until corrected or removed. The
[README](https://www.npmjs.com/package/@moq/watch) lists types and defaults.
The overlay adds play/pause, volume, fullscreen, a quality selector, a
buffering indicator, and a stats panel.

`el.audio.out.error` and `el.video.out.error` report a fatal decode error.
The failing track closes its subscription and stops decoding. Select another
source or rendition to clear the failure; the player does not resubscribe to
broken media automatically.

`el.broadcast.out.status` is `offline`, `loading`, `live`, `error`, or `unsupported`.
On `unsupported`, update the player to read the broadcast's newer catalog; the overlay shows this notice.
On `error` the origin refused the broadcast, `el.broadcast.out.error` says why,
and the player does not ask again until it is pointed elsewhere or re-enabled.

The broadcast's announcement is its online signal: the player starts when a
route serving the name is announced, goes `offline` when none is left, and
waits for one to return for as long as it is mounted. A catalog track that
ends cleanly keeps playing until the broadcast ends, so a viewer drains a
finished broadcast's tail. When the exact route
ends while a covering prefix still serves the name, it moves to the prefix. A
name outside the origin's scope is an `error`. A restarted publisher at the same name is followed at
once, with a fresh catalog, and video, audio, and the clock start over on its
timeline instead of waiting for it to catch up to the old one.

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
import * as Moq from "@moq/net";
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

    const track = active.track(name).subscribe({
        ...Moq.Track.Subscription.default(),
        priority: Hang.Catalog.PRIORITY.catalog,
    });
    effect.cleanup(() => track.close());

    const consumer = new Json.Snapshot.Consumer<unknown>({ track });
    effect.spawn(async () => {
        for (;;) {
            const state = await effect.race(consumer.latest());
            if (state === undefined) break;
            console.log("metadata", state.value);
        }
    });
});
```

Call `dispose()` on unmount. The effect re-runs when the catalog or the
active broadcast changes, so a reconnect resubscribes on its own: the
broadcast's follower moves to each start or restart, and switches only
when that resolves another broadcast. A request ending is never the trigger, so
following a restart needs a relay with discovery.

## Without the element

```ts
import * as Moq from "@moq/net";
import * as Watch from "@moq/watch";

// Shared with every other component pointed at the same relay; the broadcast
// handle reads from its origin and requests again on each announcement, so it
// follows reconnects and restarts.
const connection = new Moq.Connection({ url: new URL("https://relay.example.com/anon") });
const player = new Watch.Player({
    origin: connection.origin,
    probe: connection.probe,
    name: Moq.Path.from("alice.hang"),
    video: { canvas },
});
// player.broadcast, player.video, player.audio, player.text, player.sync,
// player.renderer, and player.emitter expose the pipeline.
// Call player.close() when playback ends.
```

Pass a signal from [`@moq/signals`](/lib/js/signals) for any control you want
to change later, such as `audio.muted` or `delay`. `Player` is the pipeline inside
`<moq-watch>`; its parts are exported for custom composition.

To chart when each audio frame arrived against when it plays, open an
`arrivals()` handle on the audio decoder. It yields every frame received while it is open, stamped
when the frame came off the transport, and a gap wherever media after an earlier
frame will never arrive (a latency skip, a hole):

```ts
const arrivals = player.audio.arrivals();
for await (const entry of arrivals) {
    if (entry.kind === "gap") {
        // Break the plotted line; nothing arrived from `timestamp` for `duration`.
        continue;
    }
    const { timestamp, arrival, late } = entry;
    // `late`: arrived after its playback deadline.
}
```

A gap's span is an upper bound: without per-frame durations it starts at the
last frame's timestamp, and it covers any pause the publisher declared.

Unread frames queue on the handle, so keep reading and keep your own window.
Leaving the loop or calling `arrivals.close()` stops observing and releases
them; nothing is retained without an open handle.

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
hold decoder memory, so a long video buffer is not. The first video frame shows
as a poster at once, and playback starts once every enabled track has buffered
the delay. `el.reset()` flushes and re-anchors at the next frame, which is how a
producer interrupts an utterance.

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
