# [L] Publisher tab reload strands watchers on the stale broadcast: ROUTE_LINGER (5s) expires before a…

## Goal

Implement and verify the behavior tracked in [#2401](https://github.com/moq-dev/moq/issues/2401)
within the issue's stated scope and boundaries.

## Plan

Use the public issue's scope, implementation notes, and acceptance criteria
below as the starting plan. Reconcile paths and assumptions with the current
tree before implementation.

### Issue context

### Publisher reload strands watchers: the 5s ROUTE\_LINGER expires before a real re-publish reconnects, forcing a full watcher teardown/rebuild (plus a missing `ended` UNANNOUNCE on close)

#### Summary

Reloading a publisher tab mid-broadcast and immediately re-publishing under the same name leaves watchers frozen on "recovering" for several seconds to ~15s before the stream resumes. Reloading the *watcher* recovers almost instantly.

The stall is a timing interaction between two layers, confirmed on the wire:

1. When a publisher's session drops, the relay treats it as a *dying session* (a non-graceful route detach) and holds the broadcast for `ROUTE_LINGER` (5s), serving from cache so a reconnecting session can resume transparently. During that window the watcher's tracks stall (no active route producing frames).
2. If the re-publish re-attaches within 5s, the watcher resumes transparently (no visible offline, ~5s of stall). If reconnect takes longer than 5s (a real 1080p60 camera re-acquire + H.264 encoder warmup + first keyframe easily does), the linger expires, the broadcast **unannounces**, and the watcher performs a full teardown and rebuild of the whole subscription chain, then re-subscribes when the publisher returns. That teardown/rebuild is the ~15s the user sees.

A distinct but related correctness gap sits underneath this: `Publisher.close()` (and the IETF publisher) never emit the `ended`/`endedId` ANNOUNCE retraction that the incremental-removal path already emits, so the relay can *only* learn a publisher stopped via transport-close/timeout detection, never via an application-level unannounce.

#### Environment

- moq-lite, ALPN `moq-lite-05`, local `moq-relay` (default `ROUTE_LINGER` 5s, `DEFAULT_IDLE_TIMEOUT` 30s, `DEFAULT_KEEP_ALIVE` 5s).
- Reproduced manually across Safari->Chrome, Chrome->Safari, Chrome->Firefox (four reproductions). Both WebTransport and the qmux/WebSocket fallback are affected.

#### Steps to reproduce

1. Publish a broadcast (`me.hang`) from browser A; watch it from browser B. Confirm the watcher is live.
2. Reload browser A's publisher tab and let it re-publish the same name.
3. The watcher shows "recovering" (stalled, 0 fps / 0 kbps) for several seconds to ~15s before resuming.
4. Contrast: reloading the *watcher* recovers near-instantly (a fresh subscriber attaches to the still-live producer and gets the next keyframe).

The manual Firefox-watcher console for step 2 shows the tell: after the reload the watcher churns a video resubscribe, then

```
subscribe close: id=4 broadcast=me.hang track=video
announced: broadcast=me.hang active=false          <- broadcast retracted (linger expired)
subscribe close: id=0 broadcast=me.hang track=catalog.json
subscribe close: id=2 broadcast=me.hang track=audio
subscribe close: id=1 broadcast=me.hang track=meta.json
```

i.e. the whole subscription is torn down (`active=false`), then re-announced and re-subscribed from scratch.

#### Mechanism

##### 1. Publisher teardown never sends `ended` (`js/net`)

`js/net/src/lite/publisher.ts` `runAnnounce()` keeps the announce stream current by diffing the broadcast map. Removing a *single* broadcast correctly retracts it:

```ts
// active.difference(newActive): removed broadcasts
if (hasAnnounceId(this.version)) {
    await encodeAnnounceBroadcast(stream.writer, { status: "endedId", id }, this.version);
} else {
    await encodeAnnounceBroadcast(stream.writer, { status: "ended", suffix: removed }, this.version);
}
```

But `Publisher.close()` bypasses that diff, setting the whole map to `undefined`:

```ts
close() {
    this.#broadcasts.update((broadcasts) => {
        for (const broadcast of broadcasts?.values() ?? []) broadcast.close();
        return undefined;
    });
    ...
}
```

and the `runAnnounce()` loop just exits on that transition, writing nothing:

```ts
const broadcasts = await Promise.race([changed, stream.reader.closed]);
if (!broadcasts) break; // no ended/endedId is ever written for the active broadcasts
```

`js/net/src/ietf/publisher.ts` has no `close()` at all; `js/net/src/ietf/connection.ts` `Connection.close()` only tears down the transport. `js/net/src/connection/reload.ts` is what invokes this on a reload: `pagehide`/`unload` -> `#suspended` -> the connect effect's cleanup -> `connection.close()` -> `Publisher.close()`. The transport close it then attempts is fire-and-forget and races page teardown, so even that is not guaranteed to arrive.

##### 2. The relay treats the drop as non-graceful and lingers 5s (`rs/moq-net/src/model/origin.rs`)

A publisher session feeds a broadcast through a `Route` (`Producer::attach_route` / `join_front`). Route teardown distinguishes a deliberate retraction from a dying session via a `graceful` flag:

```rust
// detach_route:
if graceful && s.routes.is_empty() && !s.closed {
    // Deliberate end: close now (synchronous unannounce, no linger).
    s.closed = true;
    close = true;
} else {
    // Dying session: requeue its tracks; the front lingers.
    for (name, entry) in s.served.iter_mut() { ... }
}
```

`graceful` is only set by `Route::unannounce()`, which is called when the peer *deliberately* retracts the path (sends an `ended`). The `Drop` impl uses `graceful = false`:

```rust
impl Drop for Route {
    fn drop(&mut self) { detach_route(&self.state, &self.broadcast, self.id, self.graceful); }
}
```

Because the publisher never sends `ended` (part 1), the reloaded publisher's `Route` always drops with `graceful = false`, so the broadcast takes the linger path: `run_front_janitor` holds it for `ROUTE_LINGER` (`const ROUTE_LINGER: Duration = Duration::from_secs(5)`) before unannouncing, serving from cache in the meantime.

##### 3. Two regimes

`join_front` re-attaches a re-publish to the *existing* front only while it is still live (`if s.closed { return None }`):

- **Reconnect within 5s (fast regime):** the new session joins the lingering front as a new route, `reselect()` promotes it, and its tracks resume at the first missing group. The watcher never sees `active=false`; recovery is just the new publisher's startup-to-first-keyframe.
- **Reconnect after 5s (slow regime):** the linger expires, the front closes, the broadcast unannounces. The watcher tears down its whole chain (broadcast consumer -> catalog -> video/audio decoders -> track subscribers). The subsequent re-publish is a *fresh* broadcast, so the watcher re-subscribes from scratch (fresh catalog snapshot, re-run `VideoDecoder.isConfigSupported` per rendition in `js/watch/src/video/source.ts`, new SUBSCRIBE handshake, wait for a keyframe). This is the ~15s path.

#### Evidence

Instrumented relay (`RUST_LOG=info,moq_net=debug`), headless drivers (publisher on Chrome via CDP or Firefox via Marionette; watcher on the other), fake capture devices. Numbers are from the harness, not hand-waved.

**Fast regime (reload, reconnect ~1s).** Chrome publisher reload, Firefox watcher. Relay reaped the old session in 7ms; watcher `status` stayed `live` throughout (transparent resume); recovery 4.8s (fake-media startup):

```
23:10:40.362  driver: reload publisher
23:10:40.369  WARN moq_relay: connection closed err=... closed: code=0 reason=
23:10:40.369  INFO moq_net::lite::session: session terminated
23:10:40.463  DEBUG moq_net::lite::publisher: reannounce broadcast=me.hang
```

The relay learns the broadcast is gone from `connection closed`, never from an `ended` message.

**Slow regime (crash, republish deferred to 8s to cross the linger).** Watcher timeline (t = crash):

```
t+0.6s  status=live     stalled=true   frames frozen
t+5.3s  status=offline  stalled=true   frames frozen   <- ROUTE_LINGER expired, broadcast unannounced
t+8.3s  status=live     stalled=false  frames resume    <- republish; full re-subscribe
recovery at +8.7s
```

The offline at +5.3s is the 5s linger; everything after is the teardown -> re-announce -> re-subscribe path. With a real camera whose reconnect exceeds 5s, recovery lands closer to the reported ~15s.

**Dying session detected promptly, but still served for the linger.** SIGKILL of a publisher (OS sends TCP RST, so detection is prompt) still stranded the watcher for the full linger with zero frames delivered:

```
23:20:46.030  driver: kill publisher
23:20:46.069  INFO moq_net::lite::session: session terminated   (+39ms)
... watcher status "live", videoFrames frozen ...
watcher offline at +5.3s; 0 frames delivered in the window
```

This isolates the linger: the relay knew the publisher was gone at +39ms, yet served the stale broadcast until +5.3s, because the drop was non-graceful. A deliberate `ended` (graceful) would have retracted it immediately.

**Baseline / why headless understates the symptom.** Across all clean-disconnect configs (both transports, publisher or watcher, fake media), recovery is ~5s and the watcher-reload case is near-instant. Headless browsers always emit *some* close (a WebTransport `code=0`, or a TCP RST on process death) that the relay catches within milliseconds, so automation lands in the fast regime; the full ~15s needs reconnect to exceed the linger, which real capture (camera re-acquire + encoder warmup + keyframe) does but fake devices do not.

#### Contributing watcher-side cost (`js/watch`)

When the broadcast flaps `active=false` -> `active=true` (slow regime), `js/watch/src/broadcast.ts` `#runBroadcast` does a hard teardown and rebuild with no debounce, and `js/watch/src/video/source.ts` `#runSupported` re-runs `VideoDecoder.isConfigSupported` sequentially for every rendition on the new catalog. There is no incremental resume path, so the rebuild cost is paid in full each time.

#### Secondary defect (likely separate ticket)

`js/watch/src/sync.ts`: the shared `Sync.reference` is reset only on an in-track discontinuity (`DecoderTrack.#onDiscontinuity`), never on a broadcast-level teardown/rebuild. After a reload the new stream re-zeros its timestamps (`js/publish/src/video/polyfill.ts`), so frames are compared against a stale anchor and logged as `sync[video]: N late frame(s), max Xms behind` (visible in the manual repro logs). This corrupts the jitter readout during exactly the recovery window.

#### Related defect (deliberate stop, distinct from reload)

The missing `ended` UNANNOUNCE (part 1) has its own clear cost in the *non-reload* case: when a publisher deliberately stops (closes the tab, navigates away) and does **not** come back, the relay still lingers 5s serving the dead broadcast before retracting, instead of unannouncing immediately, because it never received a graceful `ended`. The SIGKILL evidence above is exactly this case.

#### Scope

Diagnosis only; no fix proposed. The load-bearing observations are: (a) the publisher's `close()` path drops the `ended` retraction its own incremental path already knows how to send; (b) every publisher disconnect is therefore treated as non-graceful, taking the 5s `ROUTE_LINGER` path; and (c) when a real re-publish exceeds that window the watcher does a full teardown/rebuild, which is the dominant cost in the reported stall.

## Closes

- [#2401](https://github.com/moq-dev/moq/issues/2401) - close this issue when the quest finishes
