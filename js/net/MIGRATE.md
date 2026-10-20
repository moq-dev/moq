# Migrating @moq/net

How to update your code for each breaking change, newest first.

## Unreleased

### `largest()` reports within a reader's `setGroups` bounds

`Track.Subscriber.largest()` and `Track.Ordered.largest()` report the track's largest only while it is within the bounds set with `setGroups` or `withGroups`, where they used to ignore the bounds.
A largest past the cap or below the floor reports nothing; the cache never stands in for it.
A cap moved below the largest stays pending. Moving the bounds wakes a pending read, which reports the largest once it is within them and newer than the last report.
Unbounded readers and `Track.Producer.largest()` are unchanged.
To read the track's edge from a capped reader, lift the cap with `setGroups({})` first.

### Lite-07 accounts for groups with `SUBSCRIBE_DROP`

Construct `SubscribeEnd(group)` without a stream count; remove reads of `.streams` and calls to `hasStreamCount()`.
The unpublished `moq-lite-07-wip` wire restores `SUBSCRIBE_DROP` and removes Stream Count from `SUBSCRIBE_END`. Update both ends together; published lite versions keep their framing.

### `webtransport.protocols` is `versions` (#5979)

Move the list to the top-level `versions` of `connect` or `Connection`, spelled as version names: `moq-transport-22`, not the ALPN `moqt-22`; lite names are unchanged.
It now applies to the WebSocket fallback too, so list every fallback you still want, such as `versions: ["moq-lite-07-wip", "moq-lite-06"]`.
Passing `webtransport.protocols` or `websocket.protocols` throws a `TypeError`; move either to `versions`.

### Track demand reports the wanted group (#5930)

`Track.Demand.used` and `unused()` are gone.
`demand.wanted` is a `Getter<number | undefined>`: `undefined` while nobody subscribes, otherwise the group subscribers want next.
Watch it where you watched `used`, comparing against `undefined`, and wait for it to become `undefined` where you awaited `unused()`.
`Track.Info.refresh` opts a track in to new groups on join: a live-edge join asks for one only when the newest group's bytes after its first frame exceed `refresh` times that frame, not on every join.
Broadcast and group demand keep `used` and `unused()`.

### An expired group fails with `Old` even when drained (#5944)

A reader parked at the end of an unfinished group whose `maxDelay` gives up now rejects with a `StreamError` of code `StreamCode.Old`, even after reading every frame, where `readFrame()` used to resolve `undefined`.
Handle it like any other skipped group.

### Track readers follow `largest()` instead of reading `latest()` (#5725)

`latest()` on `Track.Subscriber` and `Track.Ordered` is gone, and `Subscriber.largest()` is no longer a synchronous buffered snapshot.
Await `largest()` and read `.group` where you used the sequence.
It streams the newest location, the current one at once and then each newer one, and `undefined` once the track ends.
A new group or datagram raises it at frame 0 before any frame lands, and it can name a frame the publisher reported before its group arrives.

### A timed track refuses a group that rewinds (#5803)

A local write whose first frame starts before a lower-sequence group's, or after a higher one's, throws `TimestampMismatch`.
A publisher whose timeline restarts publishes a new broadcast instead.

### `Auth.Auth` is `Auth.Handle`, and `Auth.Token` is `Auth.Auth` (#5695)

Rename `Auth.Auth` to `Auth.Handle` first, then `Auth.Token` to `Auth.Auth`.

### Superseded groups age by wall clock too (#5548)

A superseded group is stale once its successor arrived a full `maxDelay` ago, as well as by media time.
Untimed tracks are no longer exempt: with the default zero `maxDelay`, an untimed subscriber starts at the latest group and skips each superseded one.
Give it a budget to replay or tolerate reordering.

```ts
const sub = track.subscribe({ maxDelay: Milli(1000) });
```

### Time units and timers come from `@moq/clock` (#5706)

`Time.Nano`, `Time.Micro`, `Time.Milli`, and `Time.Second` are gone.
Import them from `@moq/clock`, which throws on negative or non-finite values.

### Announced requests follow restarts, and `broadcasts()` is gone

An unpinned `request(path, { announced: true })` stays open across publisher instances: its `active` moves to each new instance's broadcast and clears between them, instead of ending with `Unroutable`.
Read `active` reactively and close the request when done, or pass `epoch` to keep the old single-instance behavior.
`Origin.Producer` and `Origin.Consumer` drop `broadcasts(scope)`; iterate `announced(scope)` instead.

```ts
for await (const event of origin.announced(scope)) {
	if (event.kind === "start" || event.kind === "restart") { ... }
}
```

### Discovery follows what something watches, and `discovery` is gone

A `consume` origin stays empty until a reader watches it with `announced(scope)` or `request(path, { announced: true })`; the session then asks the relay about those prefixes only.
Code that called `request(path)` expecting a route without watching should hold an `announced(scope)` stream.
The `discovery` option on `connect`, `accept`, and `Connection`, and `Established.discovery`, are removed; passing the option throws a `TypeError`.
Drop the option.

### Announce streams yield `start`, `update`, `end`, and `restart` events

`Announce.Consumer` yields an `Announce.Event`: an `Announce.Announce` (`prefix`, `captures`, `route`) plus a `kind`.
`Announce.Update`, `Announce.Kind`, and `Announce.isActive` are gone.
Replace `Announce.isActive` with a check on `event.kind`: skip `"end"`, and treat `"start"` and `"restart"` as active.
A `"restart"` means another publisher instance now serves the prefix: request the path again.

### Routes carry the publisher epoch (#4942)

`Path.joinEpoch` and `Path.splitEpoch` are gone.
Set the epoch on the route with `announce({ epoch })` and read `Route.epoch`; pin a request to one instance with `RequestOptions.epoch`.
Nothing mints an epoch by default.

### Publishing never invents a timestamp (#5070)

`Group.Producer` and `Track.Producer` drop `writeString`, `writeJson`, and `writeBool`, and `@moq/net/zod` drops `write`.
Encode the payload yourself and write it as a frame with an explicit timestamp, or none on an untimed track.

```ts
track.writeFrame({ payload: new TextEncoder().encode(JSON.stringify(value)), timestamp });
```

### A track is timed or untimed (#4968)

`Track.Info.timescale` is optional; omitted or `null` means untimed, and `infoDefaults()` no longer fills in milliseconds.
`Group.Frame.timestamp` and `Track.Datagram.timestamp` are optional, and `appendDatagram` / `insertDatagram` take `Timestamp | undefined`.
A write whose timedness does not match its track throws `Error.TimestampMismatch`, so declare a timescale, such as `{ timescale: Timescale.MILLI }`, on every track you stamp.

### Subscriber staleness is `maxDelay`

`Track.Subscription`'s `maxAge` is `maxDelay`; `maxAge` stays the publisher's retention on `Track.Info`.
A `maxAge` key in subscription options throws a `TypeError`, even when `undefined` or beside `maxDelay`.

### Publisher retention is optional (#4242)

`Track.Info.maxAge` is optional, absent meaning no publisher limit, and `DEFAULT_MAX_AGE_MS` is gone.
Pass an explicit value such as `Milli(30_000)` where you used the constant.

### Demand is read through `demand()`

`Track.Producer` and `Group.Producer` move `used` and `unused()` to `demand()`, so `producer.unused()` is `producer.demand().unused()`.
`Allocator.reserve` takes `producer.demand()`, replacing the `Bandwidth.Demand` interface.

### A replacement track owns its start sequence (#5068)

`BroadcastProducer.insertTrack` is removed; create the track with `broadcast.createTrack(name, info)` instead.
`Track.Producer.appendGroup` and `appendDatagram` throw `duplicate sequence` instead of continuing past an earlier producer of the same name; `createTrack` now answers a queued request instead of throwing `duplicate track`.
`Request.accept` closes the request's producer with the error whenever it throws.

### Route cost is a `bigint` (#4661)

`Route.cost` is one `bigint` instead of separate warm and cold numbers.
Pass one value, such as `cost: 10n`.

### Datagrams outside the group range are skipped (#4982)

A subscriber no longer receives datagrams outside its group range, and a FETCH never returns one.

### Broadcast `close` takes no abort reason (#4152)

`broadcast.Producer.close(abort)` and `broadcast.Consumer.close(abort)` lose the parameter, and `closed` is a `GetPromise<null>`.
Call `close()` with no argument.

## 0.3 -> 0.4

### One `Connection` (#3614, #3636)

`Connection.Reload` is `new Connection({ url })`, which pools one connection per relay.
`closed` settles only on `close()`; the error that stopped retrying is `error`.

### Origins hold broadcasts (#2705, #3225)

`connection.publish(path, broadcast)` is `origin.createBroadcast(path)` then `broadcast.announce()`.
`connection.announced(prefix)` is `origin.announced(scope)`, yielding `{ prefix, kind, route }`.
`ignoreSelf` is gone: reflected announces are always dropped.

### Names mirror Rust (#3710)

| Before | After |
| --- | --- |
| `latencyMax` | `maxAge` (a `Time.Milli`) |
| `writeDatagram` | `insertDatagram` |
| `RemoteError` | `Error.Stream` / `Error.Session` |
