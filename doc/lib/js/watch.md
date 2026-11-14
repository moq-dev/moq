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
| `delay` | How far playback trails the live edge: `"auto"` (the default, sized from how late frames arrive, at most 10 s; see [audio jitter](/concept/audio-jitter)), a duration like `"300ms"`, or `"instant"` to paint video as it decodes, with no audio. |
| `lookahead` | How far ahead of the live edge media may run; see [buffered playback](#buffered-playback). |
| `stretch` | How far audio may trail the clock before it skips ahead: a duration like `"2s"` (the default), or `"0"` to turn the time stretch off; see [catching up](#catching-up). |
| `captions` | The caption track to show. |
| `visible` | Only download video while the element is on screen (or near it). |
| `backend` | How the canvas is drawn; see [@moq/video](/lib/js/video). `auto` (default), `webgpu`, or `2d`. |
| `follow` | Follow the broadcast's announcements (default on): wait for one before subscribing, so a player can mount before the stream exists, and move to each restart. Off requests the name once. |
| `catalog-format` | `hang`, `msf`, or `manual`; detected from the name by default. |
| `echo` | The viewer's name for [feedback](/concept/stats#media-reports), one path segment not starting with `.`. Once the catalog solicits feedback, the player publishes `<echo path>/<name>.echo`. Absent publishes none. |

Every attribute is also a reactive property, and property or UI changes reflect
back into the attributes. An invalid value is refused rather than defaulted: the
property setter throws, and invalid markup reports a browser `error` event and
suspends the element until corrected or removed. The
[README](https://www.npmjs.com/package/@moq/watch) lists types and defaults.
The overlay adds play/pause, volume, fullscreen, a quality selector, a
buffering indicator, and a stats panel.

Audio, video, and captions play from one clock on local time, anchored at the
earliest arrival (with `lookahead`, at the earliest retained frame once startup
fills). A congested track falls behind on its own instead of stalling the
others; in `auto` delay, its rising playout target still raises the delay
every track plays at. A discontinuity restarts only its own track's timeline,
and a track that is disabled, deselected, or fails releases its hold on the
clock.

`el.audio.out.error` and `el.video.out.error` report a fatal decode error.
The failing track closes its subscription and stops decoding. Select another
source or rendition to clear the failure; the player does not resubscribe to
broken media automatically.

A video rendition encoded with intra refresh has no keyframes, and its catalog
`warmup` says how long a decoder takes to clean up after joining. The player
decodes such a rendition twice: one decoder holds your delay and may show
refresh stripes after a join or a skip, while a second starts one group earlier
and tolerates `warmup` more lateness, so its picture is clean. The clean
picture shows whenever it has caught up. Each decoder costs its own CPU and
decoder memory, and both share one subscription. A rendition without `warmup`
uses one decoder.

An H.264 group of such a rendition opens on a recovery point that is not an
IDR. Chromium's WebCodecs refuses to open a decoder on that picture: it wants
an IDR or a recovery point that counts zero frames. So when a decoder opens on
one, the player inserts a second recovery point with a zero count before its
first slice. The slices are untouched; the stripes clear as the sweep
completes.

`el.broadcast.out.status` is `connecting`, `offline`, `loading`, `live`, `error`, or `unsupported`.
It reads `connecting` until a session could have announced the broadcast: while the
connection dials, redials, or was refused (`el.connection.error` says why), and until the
relay's initial announcements arrive. Only then does a missing broadcast read `offline`.
On `unsupported`, update the player to read the broadcast's newer catalog; the overlay shows this notice.
On `error` the origin refused the broadcast, `el.broadcast.out.error` says why,
and the player does not ask again until it is pointed elsewhere or re-enabled.

The broadcast's announcement is its online signal: the player starts when a
route serving the name is announced, goes `offline` when none is left, and
waits for one to return for as long as it is mounted. A publisher that stops
cleanly finishes its catalog and media tracks: the player plays the tail it
received, holding the last picture without a buffering spinner, then reads
`offline` (`el.video.out.ended` and `el.audio.out.ended` turn true) even while the
relay still serves the finished broadcast to late subscribers. When the exact route
ends while a covering prefix still serves the name, it moves to the prefix. A
name outside the origin's scope is an `error`. A restarted publisher at the same name is followed at
once, with a fresh catalog, and video, audio, and the clock start over on its
timeline instead of waiting for it to catch up to the old one.

## Feedback

A publisher solicits feedback with the catalog's `echo` section, such as
`<moq-publish echo-path="viewers">`. Given `echo="alice"`, the player resolves
that path against the broadcast and publishes `alice.echo` there, with one
`feedback.json` track reporting what each rendition was received and played
under its catalog ID: frames, bytes, decoded, late, decode errors, stalls and
stalled time, audio underruns, and the newest arrival, plus the PROBE
round-trip time. It writes about once a second while someone subscribes, and
follows the catalog: a removed or moved section retracts the broadcast. Each
player publishes its own broadcast, reporting only what it counted, so a second
player on one connection that resolves the same feedback path, for any catalog,
publishes nothing and warns until the first one leaves; give each player its own
name. The decoders'
`out.stats` and the stats panel read the same counters. Audio's `out.stats`
also counts `skips`, each jump that dropped buffered audio rather than playing
it faster, which the stats panel shows and the feedback track leaves out. The token must allow
publishing under the echo path.

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

    const consumer = new Json.Snapshot.Consumer<unknown>({
        track: active.track(name),
        priority: Hang.Catalog.PRIORITY.catalog,
    });
    effect.cleanup(() => consumer.close());
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
    name: Moq.Path.from("alice.hang"),
    video: { canvas },
    viewer: { viewers: connection.viewers },
});
// player.broadcast, player.video, player.audio, player.text, player.sync,
// player.renderer, and player.emitter expose the pipeline.
// Call player.close() when playback ends.
```

Pass a signal from [`@moq/signals`](/lib/js/signals) for any control you want
to change later, such as `audio.muted` or `delay`. `Player` is the pipeline inside
`<moq-watch>`; its parts are exported for custom composition.

`viewer.viewers` claims the broadcast's share of the connection's receive
estimate (see [`connection.viewers`](/lib/js/net)). Its ceiling follows the
catalog: the rate at which selection would pick the best enabled video
rendition the `video.target` allows, plus the selected audio rendition's
bitrate, counting only media being downloaded. A relay that probes raises the
estimate toward it, and video selects from the claim's grant less that audio, so a viewer on a small
rendition moves up once the grant reaches the ceiling. Players sharing a connection split its estimate
instead of each selecting against all of it. Set
`viewer.max` (`controls.viewer.max` on `<moq-watch>`) to claim a fixed ceiling
in bits per second instead while media downloads, which also caps what video selects, or `0` to claim nothing; omit `viewer.viewers` to
leave the connection's estimate unclaimed. Without a claim, video selects the best rendition its
`video.target` allows. To stop the relay probing while still adapting, keep the claim and call
`connection.viewers.setTarget(0)`.

## Catching up

After a stall, video shows each late frame as it arrives and is live again at
once. Audio cannot skip without losing what arrives late, so it slows down
slightly when the audio received ahead of it runs low, falls behind the clock,
and speeds back up once it recovers until it is back on it. `stretch` bounds
how far it may trail: only audio later than that is skipped, and the audio
subscription waits on a stalled group for `delay + stretch`, so the late audio
still arrives. A voice app might ask for `stretch="200ms"`, a sports stream for
the default 2 s, whether `delay` is `auto` or fixed.

`stretch="0"` turns the time stretch off, for content where a pitch-period
splice is audible, such as music: received audio plays unaltered, late audio
is skipped, an underrun is silence, and drift between the publisher's clock
and the device's is corrected by a small skip or pad.

## Buffered playback

By default the player minimizes latency: playback trails the live edge by the
delay, and audio catches up after a stall as [above](#catching-up). Content produced faster than real time, such as a TTS
response emitted in one burst with future timestamps, wants the opposite. Set
`lookahead` to how far ahead it may run and it plays through at the encoded pace.
If a stall leaves audio behind, it catches up by playing faster, as live
playback does, and skips only what trails by more than the larger of the
lookahead and `stretch`; a zero `stretch` still turns the stretch off:

```html
<moq-watch url="..." name="bot/tts.hang" delay="100ms" lookahead="30s"></moq-watch>
```

`lookahead` is not a jitter buffer: a live stream leaves it at zero and tunes
`delay`, since playback only skips ahead once media sits more than
`delay + lookahead` past the playhead.

Durations need a unit; a bare number other than zero is rejected. From script,
the `delay`, `lookahead`, and `stretch` properties take a `Duration` from
`@moq/clock`, such as
`el.delay = Duration.millis(300)`. Audio holds the lookahead
as encoded frames, so a large one is cheap; video waits as decoded pictures,
which hold decoder memory, so a long video lookahead is not. The first video
frame shows as a poster at once, and playback starts once every enabled track
has buffered the delay. `el.reset()` flushes and re-anchors at the next frame,
which is how a producer interrupts an utterance.

## Strict CSP

The audio worklet loads from a `blob:` URL by default, so a CSP must allow
`blob:` in `script-src`. Otherwise, copy `node_modules/@moq/watch/assets/*`
into a directory your origin serves and point the package at it before
playback starts:

```ts
import * as Watch from "@moq/watch";

Watch.assets("/moq/");
```

The URL must end with `/` and have no query or fragment. Copy the files again
on every upgrade: the worklet changes with the package. `@moq/room` plays through
`@moq/watch`, so this one call covers them.

A browser without `AudioDecoder` decodes through the libav polyfill, which
loads its worker and WebAssembly from the same directory (they are among
`assets/*`), so also allow `'wasm-unsafe-eval'` in `script-src`.
