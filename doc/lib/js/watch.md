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
| `delay` | How far playback trails the live edge: `"auto"` (derived from RTT, the default), a duration like `"300ms"`, or `"instant"` to paint frames as they decode with no pacing at all. |
| `buffer` | Future-dated media held beyond the live edge before playback skips ahead, e.g. `"30s"`. Defaults to none. |
| `captions` | The caption track to show, or absent for off. `el.text.out.available` lists the renditions for a picker. |
| `jitter` | The jitter buffer in ms. |
| `visible` | Only subscribe to video while the element is on screen: a margin (`"20%"` default, `"200px"`), `"always"`, or `"never"`. |
| `reload` | Wait for the broadcast to be announced before subscribing (default on), so a player can be mounted before the stream exists. |
| `catalog-format` | `hang` (default, from the `.hang` suffix), `hangz` (compressed), `msf`, or `manual` to supply the catalog yourself. |

The overlay adds play/pause, volume, fullscreen, a quality selector, a
buffering indicator, an unsupported-codec warning, and a stats panel.
`<moq-watch-support>` shows what the browser can play.

## Custom tracks

`broadcast.subscribeTrack(name, priority, consume)` follows the active
broadcast across reconnects for any application track, and the loose catalog
passes your own sections through to `broadcast.catalog`. Decode JSON with
`@moq/json`. Reach the pipeline from the element via `el.broadcast`,
`el.video`, `el.audio`, and `el.signals`.

## Without the element

```ts
import * as Moq from "@moq/net";
import * as Watch from "@moq/watch";

// Shared with every other component pointed at the same relay; the broadcast
// handle reads from its origin and spans reconnects.
const connection = new Moq.Connection.Shared({ url: new URL("https://relay.example.com/anon") });
const broadcast = new Watch.Broadcast({ origin: connection.origin, name: Moq.Path.from("alice.hang") });
```

`Watch.Broadcast`, `Video.Decoder`, `Video.Renderer`, `Audio.Decoder`, and
`Audio.Emitter` are the pieces the element assembles; every input and output
is a signal from [`@moq/signals`](/lib/js/signals). Load from a CDN
(`https://esm.sh/@moq/watch/element`) for a no-build embed.

## Buffered playback

By default the player minimizes latency: it skips ahead whenever media piles
up past the delay. Content produced faster than real time, such as a TTS
response emitted in one burst with future timestamps, wants the opposite. Set
`buffer` to how far ahead it may run and it plays through at the encoded pace:

```html
<moq-watch url="..." name="bot/tts.hang" delay="100ms" buffer="30s"></moq-watch>
```

Durations need a unit; a bare number is rejected. Only the delay is held as
decoded PCM; the buffer stays as encoded frames with backpressure on the
decoder, so a large one is cheap. `el.reset()` flushes and re-anchors at the
next frame, which is how a producer interrupts an utterance.
