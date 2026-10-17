# Migrating @moq/net

How to update your code for each breaking change, newest first.

## Unreleased

### A timed track refuses a group that rewinds (#5803)

A local write whose first frame starts before a lower-sequence group's, or after a higher one's, throws `TimestampMismatch`.
A publisher whose timeline restarts publishes a new broadcast instead.

### `Auth.Auth` is `Auth.Handle`, and `Auth.Token` is `Auth.Auth` (#5695)

```ts
// Before
const handle: Auth.Auth = connection.auth;
const token: Auth.Token = await handle.add(bytes);
// After
const handle: Auth.Handle = connection.auth;
const auth: Auth.Auth = await handle.add(bytes);
```

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

```ts
// Before
import { Time } from "@moq/net";
const delay = Time.Milli(250);
// After
import { Milli } from "@moq/clock";
const delay = Milli(250);
```

### Announced requests follow restarts, and `broadcasts()` is gone

An unpinned `request(path, { announced: true })` stays open across publisher instances: its `active` moves to each new instance's broadcast and clears between them, instead of ending with `Unroutable`.
Read `active` reactively and close the request when done, or pass `epoch` to keep the old single-instance behavior.
`Origin.Producer` and `Origin.Consumer` drop `broadcasts(scope)`; iterate `announced(scope)` instead.

```ts
// Before
for (const broadcast of origin.broadcasts(scope)) { ... }
// After
for await (const event of origin.announced(scope)) {
	if (event.kind === "start" || event.kind === "restart") { ... }
}
```

### Discovery follows what something watches, and `discovery` is gone

A `consume` origin stays empty until a reader watches it with `announced(scope)` or `request(path, { announced: true })`; the session then asks the relay about those prefixes only.
Code that called `request(path)` expecting a route without watching should hold an `announced(scope)` stream.
The `discovery` option on `connect`, `accept`, and `Connection`, and `Established.discovery`, are removed; passing the option throws a `TypeError`.

```ts
// Before
const connection = new Connection({ url, discovery: true });
// After
const connection = new Connection({ url });
```

### Announce streams yield `start`, `update`, `end`, and `restart` events

`Announce.Consumer` yields an `Announce.Event`: an `Announce.Announce` (`prefix`, `captures`, `route`) plus a `kind`.
`Announce.Update`, `Announce.Kind`, and `Announce.isActive` are gone.
A `"restart"` means another publisher instance now serves the prefix: request the path again.

```ts
// Before
if (!Announce.isActive(update.kind)) continue;
// After
if (event.kind === "end") continue;
if (event.kind === "start" || event.kind === "restart") resolve(event.prefix);
```

### Routes carry the publisher epoch (#4942)

`Path.joinEpoch` and `Path.splitEpoch` are gone.
Set the epoch on the route with `announce({ epoch })` and read `Route.epoch`; pin a request to one instance with `RequestOptions.epoch`.
Nothing mints an epoch by default.

```ts
// Before
origin.createBroadcast(Path.joinEpoch(path, epoch)).announce();
// After
origin.createBroadcast(path).announce({ epoch });
const request = origin.request(path, { epoch });
```

### Publishing never invents a timestamp (#5070)

`Group.Producer` and `Track.Producer` drop `writeString`, `writeJson`, and `writeBool`, and `@moq/net/zod` drops `write`.
Encode the payload yourself and write it as a frame with an explicit timestamp, or none on an untimed track.

```ts
// Before
track.writeJson(value);
// After
track.writeFrame({ payload: new TextEncoder().encode(JSON.stringify(value)), timestamp });
```

### A track is timed or untimed (#4968)

`Track.Info.timescale` is optional; omitted or `null` means untimed, and `infoDefaults()` no longer fills in milliseconds.
`Group.Frame.timestamp` and `Track.Datagram.timestamp` are optional, and `appendDatagram` / `insertDatagram` take `Timestamp | undefined`.
A write whose timedness does not match its track throws `Error.TimestampMismatch`, so declare a timescale on every track you stamp.

```ts
// Before
const track = broadcast.createTrack("chat");
// After
const track = broadcast.createTrack("chat", { timescale: Timescale.MILLI });
```

### Subscriber staleness is `maxDelay`

`Track.Subscription`'s `maxAge` is `maxDelay`; `maxAge` stays the publisher's retention on `Track.Info`.
A `maxAge` key in subscription options throws a `TypeError`, even when `undefined` or beside `maxDelay`.

```ts
// Before
track.subscribe({ maxAge: Milli(500) });
// After
track.subscribe({ maxDelay: Milli(500) });
```

### Publisher retention is optional (#4242)

`Track.Info.maxAge` is optional, absent meaning no publisher limit, and `DEFAULT_MAX_AGE_MS` is gone.

```ts
// Before
broadcast.createTrack("video", { maxAge: DEFAULT_MAX_AGE_MS });
// After
broadcast.createTrack("video", { maxAge: Milli(30_000) });
```

### Demand is read through `demand()`

`Track.Producer` and `Group.Producer` move `used` and `unused()` to `demand()`.
`Allocator.reserve` takes `producer.demand()`, replacing the `Bandwidth.Demand` interface.

```ts
// Before
await producer.unused();
// After
await producer.demand().unused();
```

### A replacement track owns its start sequence (#5068)

`BroadcastProducer.insertTrack` is removed.
`Track.Producer.appendGroup` and `appendDatagram` throw `duplicate sequence` instead of continuing past an earlier producer of the same name; `createTrack` now answers a queued request instead of throwing `duplicate track`.
`Request.accept` closes the request's producer with the error whenever it throws.

```ts
// Before
const track = new Track.Producer("chat").accept(info);
broadcast.insertTrack(track);
// After
const track = broadcast.createTrack("chat", info);
```

### Route cost is a `bigint` (#4661)

`Route.cost` is one `bigint` instead of separate warm and cold numbers.

```ts
// Before
broadcast.announce({ cost: { warm: 10, cold: 10 } });
// After
broadcast.announce({ cost: 10n });
```

### Datagrams outside the group range are skipped (#4982)

A subscriber no longer receives datagrams outside its group range, and a FETCH never returns one.

### Broadcast `close` takes no abort reason (#4152)

`broadcast.Producer.close(abort)` and `broadcast.Consumer.close(abort)` lose the parameter, and `closed` is a `GetPromise<null>`.

```ts
// Before
broadcast.close(new Error("done"));
// After
broadcast.close();
```

## 0.3 -> 0.4

### One `Connection` (#3614, #3636)

`Connection.Reload` is `new Connection({ url })`, which pools one connection per relay.
`closed` settles only on `close()`; the error that stopped retrying is `error`.

```ts
// Before
const connection = new Moq.Connection.Reload({ url });
// After
const connection = new Moq.Connection({ url });
```

### Origins hold broadcasts (#2705, #3225)

`connection.publish(path, broadcast)` is `origin.createBroadcast(path)` then `broadcast.announce()`.
`connection.announced(prefix)` is `origin.announced(scope)`, yielding `{ prefix, kind, route }`.
`ignoreSelf` is gone: reflected announces are always dropped.

```ts
// Before
connection.publish(path, broadcast);
// After
const broadcast = connection.origin.createBroadcast(path);
broadcast.announce();
```

### Names mirror Rust (#3710)

| Before | After |
| --- | --- |
| `latencyMax` | `maxAge` (a `Time.Milli`) |
| `writeDatagram` | `insertDatagram` |
| `RemoteError` | `Error.Stream` / `Error.Session` |
