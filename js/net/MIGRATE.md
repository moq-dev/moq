# Migrating @moq/net

How to update your code for each breaking change, newest first.

## Unreleased

### Absolute IETF subscriptions keep their prefix

Explicit group ranges now keep their requested start when the subscription answer
arrives after its first data. IETF publishers honor absolute and unfiltered starts
on all supported drafts instead of silently joining at the live edge. Use a live
range when only the current group is wanted. An omitted or unfiltered initial
IETF filter without a fill now includes retained history from group 0 instead
of joining live. There is no API or wire-format change.

### Relative group anchors are `bigint`

A `Track.Anchor.relative` count now uses `bigint`, like an absolute group sequence.
Write `{ relative: 2n }` instead of `{ relative: 2 }`, including the live anchor
`{ relative: 0n }`. Large relative counts round-trip exactly and resolve against
large group sequences without narrowing to a number. First-party consumers use
the new type. Frame indices and timestamps retain their bounded numeric API.

The IETF codecs also hold subgroup IDs and datagram Object IDs as `bigint`.
Unsupported subgroups and datagram objects still have the same model behavior.
Unsafe numeric error/status codes and clock waits are now refused rather than
rounded or saturated. Large IETF filter bounds remain exact bigint; their derived
frame selection bounds clamp without changing the selected content. Wire layouts and published version negotiation are unchanged.
See [wire integer precision](INTEGERS.md) for each field's representation and bounds.

### Authored tracks default to a zero `maxAge` ([#7729](https://github.com/moq-dev/moq/pull/7729))

A track created with `Broadcast.Producer.createTrack`, `Track.Request.accept`, or `Track.Producer.accept` takes a `Track.InfoInit`, whose omitted or `undefined` `maxAge` is now zero instead of no publisher limit: a superseded group is stale as soon as its successor arrives, so readers get only the live edge. The newest group never goes stale, so a track that writes one snapshot stays readable to a late joiner.
A track whose readers reach back for superseded groups (a subscription's `delay.max`, a fetch, or a range from an absolute start) passes `maxAge: null` for unlimited history, or a window, when it creates the track; changing it means a new track or broadcast. A relay or cache bounds what it keeps with its own window, not the publisher's default.
Received metadata is unchanged: a decoded `Track.Info` without `maxAge` still means no publisher limit, and `Track.infoDefaults` keeps an absent `maxAge` absent. To serve a received `Track.Info` onward unchanged, pass `{ ...info, maxAge: info.maxAge ?? null }`.

```ts
// Before: no publisher limit
broadcast.createTrack("history", { timescale: Time.Timescale.MILLI });
// After: the same, stated explicitly
broadcast.createTrack("history", { timescale: Time.Timescale.MILLI, maxAge: null });
```

### Past group ranges work over moq-transport ([#7793](https://github.com/moq-dev/moq/pull/7793))

`track.Consumer.fetchGroup()` and subscriptions to past group ranges now use standalone FETCH on drafts 18, 20, and 22. No API migration is needed. First-party publishers serve one group per FETCH; multi-group ranges require a publisher that serves them.

### IETF FETCH serves on-demand history ([#7794](https://github.com/moq-dev/moq/pull/7794))

A single-group IETF FETCH now asks `track.requests()` for groups the publisher's cache no longer holds, or never held, using the same `Track.Fill` responder as moq-lite. Existing responders need no changes. Multi-group IETF FETCH remains unsupported.

### A timestamp out of range at the track's timescale is a `TimestampMismatch` ([#7702](https://github.com/moq-dev/moq/pull/7702))

A frame or datagram whose timestamp leaves the safe range once converted to the track's timescale is refused at the write with `TimestampMismatch`, as Rust refuses it, rather than with a `RangeError` when it is encoded.
Catch `TimestampMismatch` where you caught that `RangeError`.

### A timestamp finer than the track's timescale is rounded down ([#7702](https://github.com/moq-dev/moq/pull/7702))

A timestamp that is fractional at the track's timescale, such as 1500µs on a millisecond track or a fractional `Timestamp.now()`, now goes out rounded down instead of to the nearest unit: 1500µs is 1 ms, not 2. Rust's `Timestamp::convert` and `track::Info::rescale` already round down, and `Track.rescale` does too.
Write timestamps at the track's own scale where the exact value matters.

### Group sequences are `bigint` ([#7681](https://github.com/moq-dev/moq/pull/7681))

Every group sequence is a `bigint`, so a group ID past 2^53 decodes, relays, and encodes again instead of being refused: `Group.Producer`/`Consumer.sequence`, `Track.Position.group`, `Track.Bound`, an `absolute` `Track.Anchor`, `Track.Datagram.sequence`, `Track.Demand.wanted`, the `group` of `Ordered.readFrame()`, and the `sequence` that `createGroup`, `insertDatagram`, and `fetchGroup` take and `appendDatagram` returns.
Write literals with an `n` (`track.createGroup(5n)`, `{ absolute: 10n }`), convert with `BigInt(n)`, and compare with `===` against another bigint: `5n === 5` is `false`. `Math.min` and `Math.max` refuse a bigint, and `JSON.stringify` throws on one.
Frame indices, a `relative` anchor's count, and timestamps stay `number`. A number where a sequence belongs throws a `TypeError`, and a negative one a `RangeError`.

### `Subscription.maxDelay` is `delay: Track.Delay` ([#7597](https://github.com/moq-dev/moq/pull/7597))

`Track.Subscription` carries a `delay: Track.Delay` (`{ min?: Milli | null; max?: Milli | null }`) in place of `maxDelay`, which is now refused at runtime.
Replace `maxDelay: d` with `delay: { max: d }`. `max` keeps `maxDelay`'s meaning, and an omitted field is zero.
`min` is the subscriber's target latency; `null` means not stated, which only a peer on moq-lite before 07 or moq-transport sends. moq-lite-07 SUBSCRIBE and SUBSCRIBE\_UPDATE carry it, and a track's aggregate takes the smallest stated one.
`FetchGroupOptions.maxDelay` is unchanged.

### `Subscription.groups` is `ranges` ([#6949](https://github.com/moq-dev/moq/pull/6949))

`Track.Subscription` asks for `ranges: Track.Range[]` in place of `groups`, which is now refused at runtime. Each `Track.Range` starts and ends at a `Track.Anchor`: an `absolute` group, one `relative` to the latest, or the group containing a `time`.
Replace `groups: { start: { included: a }, end: { excluded: b } }` with `ranges: [{ start: { absolute: a }, end: { absolute: b } }]`. An inclusive end `b` becomes `absolute: b + 1`, and an excluded start `a` becomes `absolute: a + 1`.
A reader reads only its own ranges, whatever another subscriber widens the request to. `withRange` only narrows them; widen with `update({ ranges })`.

### Bounded reads end ([#6949](https://github.com/moq-dev/moq/pull/6949))

A `recvGroup` or `nextGroup` whose ranges all end now returns `undefined` once every group in them was handed out, dropped, skipped as stale, or will never arrive, as Rust's reads do since [#7545](https://github.com/moq-dev/moq/pull/7545). The track goes on: replacing the ranges with `update` reads on from the same subscriber. A `withRange` cap holding a group back keeps the read open, since the cap may rise.
A loop that read a bounded range until a timeout now ends on its own; one that waited for more groups past the end of its ranges must widen them first.

### Ordered subscriptions wait for owed groups

An ordered reader on a subscribed track now waits at a missing group while the feed may still deliver it, up to `delay.max` by media time or wall clock. Local tracks still skip gaps, and the zero default waits for nothing.
With `delay: { max: null }`, there is no subscriber deadline. Use a finite budget for bounded waiting: a moq-transport SUBSCRIBE signals no gaps, so an unlimited wait can last until the track ends. The publisher's `Track.Info.maxAge` still bounds the budget.

### `maxDelay: null` sets no subscriber limit ([#7403](https://github.com/moq-dev/moq/pull/7403))

A `Track.Subscription` whose `maxDelay` is `null` reaches back as far as the publisher's `Track.Info.maxAge` allows, as `None` does in Rust. An omitted or `undefined` `maxDelay` is still zero.

### Paths fit moq-transport's 4096-byte namespace ([#6927](https://github.com/moq-dev/moq/pull/6927))

`Path.MAX_BYTES` is 4096, moq-transport's namespace limit, so every path fits every wire and an origin refuses a longer one. Keep broadcast paths within 4096 bytes.
A moq-transport namespace whose escaped path would exceed it is refused when decoded, and a SUBSCRIBE, TRACK\_STATUS, or PUBLISH whose namespace and track name exceed 4096 bytes together fails that request instead of being sent.

### `Path.from` refuses empty segments ([#6648](https://github.com/moq-dev/moq/pull/6648))

`Path.from` throws on a component with a leading, trailing, or doubled `/` instead of collapsing it, so a path has one spelling, and `Path.decode` refuses one off the wire. A moq-transport namespace field ending in `/`, which escapes to an empty segment, is refused too.
Pass canonical components: `Path.from("room/cam")` or `Path.from(prefix, "cam")`, where an empty component is the root, rather than ``Path.from(`${prefix}/cam`)`` or `Path.from("room/")`.

### moq-transport drafts 14 through 17, 19, and 21 are removed ([moq-dev/moq#6753](https://github.com/moq-dev/moq/pull/6753), [moq-dev/moq#6754](https://github.com/moq-dev/moq/pull/6754))

A peer offering only removed drafts fails the handshake.
The moql SETUP (no ALPN, or `moql`) negotiates only moq-lite versions.
Peers must speak moq-transport draft 18, 20, or 22, or moq-lite.

### Request handlers are `requests()` ([#6752](https://github.com/moq-dev/moq/pull/6752))

Rename `Origin.Producer.dynamic(prefix)` to `requests(prefix)`, and the `Origin.Dynamic` handle it returns to `Origin.Requests`; its methods are unchanged.

### Names say one thing ([#6751](https://github.com/moq-dev/moq/pull/6751))

`Auth.Auth` is `Auth.Credential`, and `Reservation.grant` is `Reservation.rate`.
A connection's `bandwidth` allocator is `allocator`, mirroring Rust's `Session::allocator()`.
`Track.Producer.writeGroup(group)` is `createGroup(sequence)`, which creates the group and returns it like `appendGroup()`: replace `const group = new Group.Producer(n); track.writeGroup(group);` with `const group = track.createGroup(n);`.
A group can no longer be written before it joins a track, or join more than one track.

### One lifecycle vocabulary ([#6750](https://github.com/moq-dev/moq/pull/6750))

`Group.Producer.close(abort?)` and `Track.Producer.close(abort?)` split by meaning: call `finish()` for a clean end and `abort(err)` for an error. On a group, `finish()` is terminal and an `abort` after it is a no-op. On a track, `finish()` only declares the end, so groups below it stay writable; then call `close()` to release the producer, which aborts with a `Cancel` stream error if no end was declared. Replace `track.close()` with `track.finish(); track.close();`.
`Track.Producer.closed` and `Track.Demand.closed` settle once the outcome is final: `null` when the declared end is reached with every group below it finished, even while the producer stays open, or the abort error.
`Group.Consumer.close()` takes no error and releases only that handle; other readers and the producer are unaffected.
`Announce.Consumer.close()` takes no error and releases only that handle. A hand-made `Announce.Producer` watches its new `wanted` getter, which turns false once the last consumer closes, instead of its `closed`.
The last `Broadcast.Consumer.close()` no longer closes a produced broadcast; only its producer's `close()` does. Close the producer when you are done with it. A broadcast created on an origin closes when a new `createBroadcast` supersedes it at its path, or when the origin or scope that created it closes, even while a consumer handle holds it. Tracks already subscribed keep serving until they end.
`Group.Demand.unused()` and `Broadcast.Demand.unused()` reject once the group or broadcast closes, instead of resolving.
The wire layer's `Lister.ended` is `closed`.
`Track.Subscriber.end()` and `Track.Ordered.end()` are gone; `await finished()` instead.

### Publication handles own announcement control

`Origin.Producer.createBroadcast(path)` and `Origin.Prepared.publish()` return an `Origin.Publication`.
Keep that handle for `announce(route)`, `unannounce()`, and `route`; fill tracks through its readonly `broadcast` producer.
`Broadcast.Producer` no longer exposes announcement methods or route metadata, including standalone producers and `prepared.broadcast` before admission.
Closing `publication.broadcast` still retracts and ends the broadcast. Keeping only that fill handle leaves the broadcast and its announcement live; releasing a publication reference does not retract it.

```ts
const publication = origin.createBroadcast(path);
const broadcast = publication.broadcast;
broadcast.createTrack("data");
publication.announce();
```

### `Varint.sizeLeadingOnes` is gone

Take the length of `Varint.encodeLeadingOnes(v)`.

### Published names refuse navigation segments

Broadcast creation and route publication now refuse literal `.` and `..` segments, including segments in the producer's root.
Resolve relative references before publishing, or rename broadcasts that used these segments.
Relative references and subscription paths retain their semantics.

### A listing says whether a removed track finished (#6399)

`Broadcast.TracksEvent` gains `{ kind: "finished", name }`: the track left the listing because it finished. `{ kind: "removed", name }` now means it left unfinished. Handle both where you matched `"removed"`.

### `Track.Producer.largest()` is a getter

`Track.Producer.largest()` returns the track's largest location synchronously, `undefined` before any, instead of a promise for the next newer one.
Drop the `await`. Every caller reads the same value: the old shared cursor let one caller take an update and leave another waiting forever.
To wait for each newer one, keep a reader and follow its stream: `const reader = producer.subscribe();`, then `await reader.largest()`.

### A frameless group is not a successor (#6111)

A group followed by a frameless one is judged stale against the next group that has a frame, where the frameless group used to count.

### Priorities are signed (#6142)

`Track.Info.priority`, `Track.Subscription.priority`, and a fetch's `priority` take `-128..=127`, higher first, and 0 is the unset midpoint; anything outside throws a `RangeError`.
The track's default was 127, the midpoint, and the subscription's was 0, the least urgent; both are 0 now.
To keep a value's moq-lite byte, subtract 128 from it: 255 becomes 127, 128 becomes 0, and 0 becomes -128.
The wire messages (`Lite.Subscribe`, `Lite.TrackInfo`, `Ietf.Subscribe`, ...) hold the same signed priority, mapped to the wire byte when encoded.

### A dropped or cut group ends its reader with an error (#6143)

A group the publisher cut, or named in `SUBSCRIBE_DROP`, after a `Track.Subscriber` saw it is now handed out, and its reads reject with the publisher's `StreamError`; before, it was skipped or ended cleanly.
A group already held rejects the same way when the drop lands, even while its stream is still arriving.
On lite-05 and lite-06 the code is always `StreamCode.Old`.
Treat a rejected group read as the loss of that group and keep reading the track, rather than ending the subscription.

### A claim answers by publishing (#5678)

`Origin.Producer.dynamic(prefix)` takes no route: an application claim has no instance, cost, or hops.
Answer each `Origin.Request` with `using prepared = request.prepare()`, fill `prepared.broadcast`, then `prepared.publish()`, which announces the broadcast at the exact path under a fresh instance of its own and returns its publication handle.
Only `publish` announces it: `prepared.broadcast` has no announcement methods.
`Prepared.reject(err)` is replaced by idempotent `close()` and `[Symbol.dispose]()`; either refuses the unpublished answer as `Unroutable` and closes its broadcast. Closing that broadcast also refuses the request. `using` handles normal scope exit and exceptions; after `publish()`, disposal leaves the returned publication and its fill producer live.
Use `request.reject(err)` to refuse before preparing. A retained, unclosed `Prepared` keeps its request pending; garbage collection provides no prompt cleanup guarantee.
Construct a `Prepared` only with `request.prepare()`. Its constructor and the `Request.publisher`, `publishPrepared`, and `rejectPrepared` completion hooks are no longer public.
`Dynamic.update` is no longer part of the public API (#6496).
`Request.serve(broadcast)` additively exposes the former private answer path: serve held content without announcing an exact route. The prefix route promises the same bytes and continuous sequences at each served path; start a new prefix route for different content, or prepare and publish an independent job.
Close the claim and claim again to change it, and close an answer's producer to end that output.

### `largest()` reports within a reader's `setGroups` bounds

`Track.Subscriber.largest()` and `Track.Ordered.largest()` report the track's largest only while it is within the bounds set with `setGroups` or `withGroups`, where they used to ignore the bounds.
A largest past the cap or below the floor reports nothing; the cache never stands in for it.
A cap moved below the largest stays pending. Moving the bounds wakes a pending read, which reports the largest once it is within them and newer than the last report.
Unbounded readers are unchanged.
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
A group received over moq-lite-07 is checked the same way.
A track received over moq-lite-05/06 keeps its timestamps unchecked, since they may be an untimed origin's send times, and so does one received over moq-transport.
A relay serves both on over every version, where a moq-lite-07 receiver checks them and may refuse a group that rewinds.
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

### `follow(path)` replaces `request(path, { announced: true })`, and `broadcasts()` is gone ([#7173](https://github.com/moq-dev/moq/pull/7173), [#7476](https://github.com/moq-dev/moq/pull/7476))

`RequestOptions.announced` is removed: replace `request(path, { announced: true })` with `follow(path)`, which returns an `Origin.Following`.
An unpinned `follow(path)` stays open across publisher instances: its `active` moves to each new instance's broadcast and clears between them, instead of ending with `Unroutable`.
Read `active` reactively and close the follower when done, or use `request(path)` to stay on one instance.
A follower needs discovery: a session without it never serves one.
`Origin.Producer` and `Origin.Consumer` drop `broadcasts(scope)`; iterate `announced(scope)` instead.

```ts
for await (const event of origin.announced(scope)) {
	if (event.kind === "start" || event.kind === "restart") { ... }
}
```

### Discovery follows what something watches, and `discovery` is gone

A `consume` origin stays empty until a reader watches it with `announced(scope)`, `request(path)`, or `follow(path)`; the session then asks the relay about those prefixes only.
`request(path)` owns the discovery of its path until closed, so it finds a broadcast the relay serves with nothing else watching first, and retries as the routes covering the path change.
It fails `Unroutable` once at least one session is attached and every session's discovery has synced without a route for it; with no session attached, such as between reconnects, it waits. Drop any `announced(scope)` stream held only so a request could resolve.
The `discovery` option on `connect`, `accept`, and `Connection`, and `Established.discovery`, are removed; passing the option throws a `TypeError`.
Drop the option.

### Announce streams yield `start`, `update`, `end`, and `restart` events

`Announce.Consumer` yields an `Announce.Event`: an `Announce.Announce` (`prefix`, `captures`, `route`) plus a `kind`.
`Announce.Update`, `Announce.Kind`, and `Announce.isActive` are gone.
Replace `Announce.isActive` with a check on `event.kind`: skip `"end"`, and treat `"start"` and `"restart"` as active.
A `"restart"` means another publisher instance now serves the prefix: request the path again.

### Routes carry an instance and a per-instance epoch (#4942, [#7489](https://github.com/moq-dev/moq/pull/7489))

Every route carries an `Instance`: non-empty text naming one continuous run of group numbers at its path. Every broadcast mints a random one and keeps it across re-announcing, so most publishers set nothing.
A publisher that continues its group sequences across restarts may name a stable one with `announce({ instance, epoch })`, raising `epoch` on each run.
Read `Route.instance` and `Broadcast.Consumer.instance`; pin a request or follower to one with `request(path, { instance })` or `follow(path, { instance })`.

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

`Track.Subscription`'s `maxAge` is `maxDelay`; `maxAge` stays the publisher's staleness bound on `Track.Info`.
A `maxAge` key in subscription options throws a `TypeError`, even when `undefined` or beside `maxDelay`.

### `Track.Info.maxAge` is optional (#4242)

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

`connection.publish(path, broadcast)` is `origin.createBroadcast(path)` then `publication.announce()` on the returned handle.
`connection.announced(prefix)` is `origin.announced(scope)`, yielding `{ prefix, kind, route }`.
`ignoreSelf` is gone: reflected announces are always dropped.

### Names mirror Rust (#3710)

| Before | After |
| --- | --- |
| `latencyMax` | `maxAge` (a `Time.Milli`) |
| `writeDatagram` | `insertDatagram` |
| `RemoteError` | `Error.Stream` / `Error.Session` |
