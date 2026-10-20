# Migrating moq-net

How to update your code for each breaking change, newest first.

## Unreleased

### Each group has one writer (#6020)

`group::Producer` no longer implements `Clone`: keep one writer and move it by value between owners, or put it behind a lock when several callers write it.
Use `consume()` to read and `demand()` to watch readers or closure with `closed()`.
`Deref<Target = group::Info>` remains, so `producer.clone()` can now copy the `Info` instead of a writer and fail at a later call; remove those clones explicitly.
`finish` now takes `&mut self`. `Error::FrameOpen` is removed because owning the writer prevents concurrent frame writes or finishing with an open frame.
Dropping an unfinished writer now aborts the group with `Error::Dropped` immediately, even while the track cache retains the group.
`group::Producer` is `#[must_use]`, so a discarded `track.append_group()?;` now warns: finish the group, or hold the writer while it stays open.

### Ranges filter each subscriber's reads (#5929)

A `track::Subscriber` now reads only the groups its own `Subscription::ranges` cover, resolved again whenever `update` replaces them; before, they only set where its reads started.
A reader that relied on groups another subscriber asked for must widen its own ranges.
Once a track finishes, a subscriber with nothing left under its `set_groups` cap ends rather than waiting for the cap to rise; raise it before the track finishes to read past it.

### `largest()` reports only upstream state, within a reader's `set_groups` bounds

`track::Subscriber::largest()` and `track::Ordered::largest()` (and their `poll_largest`) report the track's largest only while it is within the bounds set with `set_groups`, where they used to ignore the bounds.
A largest past the cap or below the floor reports nothing; the cache never stands in for it.
A cap moved below the largest stays pending. Moving the bounds wakes a pending read, which reports the largest once it is within them and newer than the last report.
Every `largest()`, `track::Producer` and `track::Consumer` included, now waits while a relay's copy is idle and reports once its new upstream subscription is live, rather than the largest an ended feed left.
Cached groups are still served meanwhile.
To read the track's edge from a capped reader, lift the cap with `set_groups(..)` first.

### Lite-07 accounts for groups with `SUBSCRIBE_DROP`

`lite::SubscribeEnd` now contains only `group`; remove `streams` from struct literals and stop calling `Version::has_stream_count()`.
The unpublished `moq-lite-07-wip` wire restores `SUBSCRIBE_DROP` and removes Stream Count from `SUBSCRIBE_END`. Update both ends together; published lite versions keep their framing.

### Track demand reports the wanted group (#5930)

`track::Demand::used`, `unused`, `is_used`, `poll_used`, and `poll_unused` are gone.
`wanted()` returns `None` while nobody consumes the track and `Some(n)` while consumers want group `n` or later; open a group whenever `n` passes your largest.
`wanted_changed().await` (or `poll_wanted_changed`) returns it at once on the first call, then on each change, and errors once the track closes.
A clone of a `Demand` watches on its own.

```rust
// Before
demand.used().await?;
demand.unused().await?;
// After
while demand.wanted_changed().await?.is_none() {}
while demand.wanted_changed().await?.is_some() {}
```

`track::Info::refresh` opts a track in to new groups on join: set it with `Info::with_refresh(ratio)`.

### An expired group fails with `Old` even when drained (#5944)

A reader parked at the end of an unfinished group whose max delay gives up now gets `Error::Old`, even after reading every frame, where it used to end cleanly.
`group::Consumer::finished` reports the same error.
Handle it like any other skipped group.

### Track readers follow `largest()` instead of reading `latest()` (#5725)

`latest()` on `track::{Producer, Consumer, Subscriber, Ordered}` is gone.
Await `largest()` and read `.group` where you used the sequence; where you read the edge without waiting, poll `poll_largest` once with `kio::Waiter::noop()`.
It streams the newest location, the current one at once and then each newer one, and `None` once the track ends.
A new group or datagram raises it at frame 0 before any frame lands, and it can name a frame the publisher reported before its group arrives.

### Session stats are keyed by label, not auth root (#5731)

`stats::Handle::session`, `stats::Handle::request`, and `stats::Handle::refuse` take the stats label where they took the auth root.
Pass the empty path for a session without one.
`Session::set_tier` is now `Session::retag`, which also moves the session to a new label.
`SessionEntry::root` is renamed `SessionEntry::label`.

```rust
// Before
let session = handle.session(&root);
session.set_tier(tier);
// After
let session = handle.session(&label);
session.retag(tier, &label);
```

### Subscriptions carry group ranges (#5732)

`track::Subscription::start` and `end` are replaced by `ranges: Vec<Range>`, and `with_start` and `with_end` are gone.
Use `with_groups(start..end)` for one span of groups, or `with_ranges` with `Range::live()`, `Range::oldest()` (the default), or `Range::groups(..)`.

### Receive estimates come from `Session::recv_bandwidth` (#5777)

The connection stats no longer carry a receive estimate.
Hold the consumer `Session::recv_bandwidth` returns to request PROBE, read it with `peek()` or wait with `changed()`, and drop it to stop.

```rust
let bandwidth = session.recv_bandwidth();
let rate = bandwidth.as_ref().and_then(|b| b.peek());
```

### A timed track refuses a group that rewinds (#5803)

A local write whose first frame starts before a lower-sequence group's, or after a higher one's, fails with `TimestampMismatch`; `@moq/net` throws it the same way.
Received groups are accepted as is, since their timestamps may be send times.
A publisher whose timeline restarts, such as an HLS `EXT-X-DISCONTINUITY` that resets decode times, publishes a new broadcast instead.

### `broadcast::Producer::finish` is back, and lingers (#5795)

`finish()` ends the broadcast cleanly and returns a `Finish` that keeps its finished tracks servable until their media is stale.
Dropping the `Finish` closes the broadcast, so `close()` still ends it now.

```rust
// Close now
producer.close();
// Serve late subscribers until the media is stale
producer.finish().wait().await;
```

### `auth::Token` is `auth::Auth` (#5695)

Rename `auth::Token` to `auth::Auth`.
`auth::Handle::add` returns an `auth::Auth`, which is `Clone` and compares by identity, so a route can name it with `origin::Route::with_auth`.

### Custom transports open and send with `SendOptions` (#5770, #5872)

`transport::poll::Session::send_datagram` replaces `poll_send_datagram`.
It takes the payload as `Bytes` and a `SendOptions { group, priority }`, and never blocks.
`poll_open_uni` and `poll_open_bi` take the `SendOptions` the new stream starts with, as do the `open_uni` and `open_bi` helpers.
`SendOptions::default()` is ungrouped at priority 0.

```rust
fn poll_open_uni(
	&mut self,
	cx: &mut Context<'_>,
	options: SendOptions<'_, <Self::SendStream as SendStream>::Group>,
) -> Poll<Result<Self::SendStream, Self::Error>> { ... }

fn send_datagram(
	&mut self,
	payload: Bytes,
	options: SendOptions<'_, <Self::SendStream as SendStream>::Group>,
) -> Result<(), Self::Error> { ... }
```

### A bare track handle is not demand (#5820)

Holding a `track::Consumer` sends nothing and no longer keeps the producer used.
Only a query, subscription, or fetch made from it counts, until it is dropped.
Hold one of those, such as `track.subscribe(None)` or `track.query()` to only learn its info, for as long as you want the track.

### Dynamic broadcasts see requests when they are made (#5820)

`broadcast::Consumer::track` no longer enqueues a request.
`broadcast::Producer::requested_track` yields it once the handle queries, subscribes, or fetches.

### Custom transports declare send groups (#5711)

A `transport::poll::SendStream` declares `type Group` (`Ungrouped` when the transport has none) and implements `set_group`, and `set_priority` takes an `i64`.
A `transport::poll::Session` implements `send_group`, returning `None` without groups.

```rust
type Group = Ungrouped;
fn set_priority(&mut self, priority: i64) { ... }
fn set_group(&mut self, group: &Ungrouped) { match *group {} }
// and on the Session
fn send_group(&mut self, _priority: i64) -> Option<Ungrouped> { None }
```

### Superseded groups age by wall clock too (#5548)

A superseded group is stale once its successor arrived a full `max_delay` ago, as well as by media time.
Untimed tracks are no longer exempt: with the default zero `max_delay`, an untimed subscriber starts at the latest group and skips each superseded one.
Give it a budget to replay or tolerate reordering.

```rust
let sub = track::Subscription::default().with_max_delay(Duration::from_secs(1));
```

### `cache::Pool::gc` takes a waiter

Call `pool.gc(now, &waiter)`, and call it again when the waiter wakes, since a parked read can arm an earlier deadline than the one returned.

### A replacement track owns its start sequence (#5068)

`append_group` and `append_datagram` on a track that replaces an earlier producer of the same name return `Error::Duplicate` instead of continuing past the earlier producer's sequence.
Pick the start sequence yourself with `create_group(group::Info { sequence })`, or publish a new broadcast.

### Announcements are `Start`, `Update`, `End`, and `Restart` events

`announce::Consumer` yields `announce::Event`, carrying an `announce::Announce { prefix, captures, route }`.
`announce::Update` and `announce::Kind` are gone.
`Restart` means another publisher instance now serves the prefix: request the path again.
A newer epoch no longer ends open subscriptions with `Unroutable`; they stay on the old instance until dropped or its route goes.

```rust
while let Some(event) = announced.next().await {
	match event {
		announce::Event::Start(a) | announce::Event::Restart(a) => {
			let broadcast = origin.request_broadcast(&a.prefix, a.route.epoch).await?;
		}
		announce::Event::Update(_) => {}
		announce::Event::End(_) => {}
	}
}
```

### Routes carry the publisher epoch (#4942)

An epoch is a property of the route, not the path.
`Path::join_epoch` and `Path::split_epoch` are gone; publish the plain path with `route.with_epoch(epoch)` and read `Route::epoch`.
Nothing mints an epoch by default, and a route without one does not resume across routes.

### `request_broadcast` takes an epoch

Pass `None` to accept any publisher instance, or an `Epoch` to refuse every other one.
A dynamic route updated to another epoch refuses the requests its handler still holds with `Unroutable`.

### Demand is read through `demand()`

`track::Producer`'s `is_used`, `used`, `unused`, and `poll_unused`, and `group::Producer`'s `used` and `unused`, moved to `demand()`: `track.unused().await` is `track.demand().unused().await?`.
`track::Request`, `track::Dynamic`, and `group::Request` poll it through `demand().poll_unused`, which returns `Poll<Result<()>>`: treat `Err` (the request closed) as unused too.
A `group::Request` is withdrawn once its last `fetch_group` caller leaves, so a handler that sees it unused just drops it; a later fetch queues a fresh request, and serving the abandoned one returns `Error::Duplicate`.

### moq-net owns its transport traits

`moq_net::web_transport_trait` is gone.
`transport::poll::{Session, SendStream, RecvStream}` carry their own `poll_*` methods, `transport::Error`, and `transport::Stats`, and `Error::from_transport` takes a `transport::Error`.
A custom transport implements these traits and reports `binding()`: `transport::Binding::Uri` when its handshake carries the request URI, or `InBand` when SETUP carries it.

```rust
impl moq_net::transport::poll::Session for MySession {
	fn binding(&self) -> moq_net::transport::Binding {
		moq_net::transport::Binding::Uri
	}
	// poll_accept_bi, poll_open_uni, ...
}
```

### Data publishes keep optional timestamps

`Timed<P>` drops its clock type parameter and its `at` is an `Option<Timestamp>`.
Build it as `Timed { value, at: Some(at) }`.
Publish `at: None` only on an untimed track; pass a timestamp on a timed one.

### A track is timed or untimed

`track::Info.timescale` is `Option<Timescale>`, `None` for an untimed track, and an undeclared timescale no longer defaults to milliseconds.
`Frame.timestamp`, `frame::Info.timestamp`, and `Datagram.timestamp` are `Option<Timestamp>`, and `group::{Producer, Consumer}::timescale()` return `Option<Timescale>`.
A write whose timedness does not match its track fails with `TimestampMismatch`.
Tracks from moq-lite before 05 and moq-transport without `TIMESCALE` arrive untimed instead of stamped with their arrival time.
To keep millisecond timestamps, pass `track::Info::default().with_timescale(Timescale::MILLI)` to `create_track` instead of `None`.

### Subscriber staleness is `max_delay`

How far a group may fall behind the live edge before a subscriber skips it is no longer named `max_age`, which stays the publisher's retention on `track::Info`.
Call `track::Subscription::with_max_delay` instead of `with_max_age`.

### Publisher retention is optional (#4242)

`track::Info::max_age` is `Option<Duration>`, `None` meaning no publisher limit.
`track::DEFAULT_MAX_AGE` and `origin::Config::default_max_age` are gone: pass your own `Duration` to `with_max_age`.

### Route cost is one value (#4661)

`Cost` is opaque: construct it with `Cost::new` and read it with `value()`.
The `warm` and `cold` fields are gone.

### Datagrams are unfetchable and bounded by the group range (#4982)

A FETCH that reaches a datagram fails with `Error::NotFetchable` (received as `Error::Stream(StreamError::NotFetchable)`).
Datagrams outside a subscriber's group range are skipped.
Treat both like `NotFound` if you match on errors.

### Sessions cap peer requests (#4820)

A peer past the session's announce, subscription, or fetch bound loses the session with `TooManyRequests`.
Raise the defaults for a large relay mesh.

```rust
let client = Client::new().with_limits(session::Limits { subscriptions: 100_000, ..Default::default() });
```

### A path is one broadcast (#4741)

Broadcast identity is the path, whatever the publisher or first hop.
A publisher that restarts its group numbering must publish under a new broadcast name.

### Broadcast `finish` and `abort` are removed (#4152)

Close a broadcast with `close()`; its end carries no cause.
`broadcast::Consumer::is_finished` is gone, and `closed()` on `Consumer` and `Dynamic` returns `()`.

### No `VarInt`

Varints are plain `u64`s.
Use `moq_net::varint::decode_quic(buf)` and `moq_net::varint::encode_quic(v, buf)`, which fails past `varint::MAX_QUIC`.

## 0.2 -> 0.3

### Origins (#3400, #3804)

`create_broadcast(path, route)` is `origin.publish(path, route)?`, or `create_broadcast(path)` then `broadcast.announce(route)`.
`with_root` plus `scope(prefixes)` is one `scope(root, &patterns)` returning `Result`.
`origin::Info` is `origin::Config`.

### Announcements are prefix routes (#3225, #3770)

`announce::Update` is `{ prefix, route, kind, captures }`.
Skip inactive updates and resolve the broadcast through the origin.
Serve a subtree on demand with `origin.dynamic(prefix, route)`.

```rust
if !update.kind.is_active() {
	continue;
}
let broadcast = consumer.request_broadcast(&update.prefix).await?;
```

### Track renames (#3666)

| Before | After |
| --- | --- |
| `with_latency_max` / `latency_max` | `with_max_age` / `max_age` |
| `write_datagram(Datagram)` | `insert_datagram(sequence, timestamp, payload)` |
| `track::SubscriberControl` | `track::Control` |
| `track::GroupRequest` | `group::Request` |
| `ConnectionStats` | `session::Stats`, with `estimated_send_rate` / `estimated_recv_rate` |

### Oversized groups abort (#3585)

A group past its size limit aborts with `GroupTooLarge` instead of shedding its head.
