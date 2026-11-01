# Migrating moq-net

How to update your code for each breaking change, newest first.

## Unreleased

### `broadcast::Demand` reads like `track::Demand`

Replace `is_used()` with `wanted()`, and `used()`, `unused()`, `poll_used`, and `poll_unused` with a loop over `wanted_changed()` (or `poll_wanted_changed`), which returns the new value each time it changes and errors once the broadcast closes.
`broadcast::Consumer::demand()` is gone: call `demand()` on the broadcast's `Producer`.

```rust
// Before
demand.used().await?;
demand.unused().await?;
// After
while !demand.wanted_changed().await? {}
while demand.wanted_changed().await? {}
```

A group a subscription or fetch handed out now keeps its track wanted until the group finishes and a reader reads its end (a range cap is not the end): until then `track::Demand::wanted` stays `Some` and `track::Producer::abort_unused` declines. Drop the group, or read it to its end, to let the demand go.

### Paths fit moq-transport's 4096-byte namespace ([#6927](https://github.com/moq-dev/moq/pull/6927))

`Path::MAX_BYTES` is 4096, moq-transport's namespace limit, so every path fits every wire and building a longer one fails with `path::Error::TooLong`. Keep broadcast paths within 4096 bytes.
A moq-transport namespace whose escaped path would exceed it is refused when decoded, and a SUBSCRIBE, TRACK\_STATUS, FETCH, or PUBLISH whose namespace and track name exceed 4096 bytes together fails that request with `Error::Encode` instead of being sent.

### Building a `Path` refuses empty segments ([#6648](https://github.com/moq-dev/moq/pull/6648))

`Path::new` and `TryFrom` fail with `path::Error::EmptySegment` on a leading, trailing, or doubled `/` instead of collapsing it, so a path has one spelling; the empty path is still the root. Origin entry points report it as `Error::EmptySegment`, and `Path::from_static` panics on it.
Pass a prefix without a trailing `/`: `Path::from_static("room")` covers `room/cam`. Build a path from pieces with `Path::join` rather than `format!`, since an empty piece is the root and adds no segment.
`Path::from_escaped` is gone, and `path::TooLong` is `path::Error::TooLong`. `Path::head(n)` returns the path's first `n` segments.
moq-lite decoding refuses a path with an empty segment as a protocol violation, and moq-transport decoding refuses a namespace field ending in `/`, which escapes to one.

### moq-transport drafts 14 through 17 are removed ([#6753](https://github.com/moq-dev/moq/pull/6753))

`ietf::Version::Draft14` through `Draft17` are gone, and a peer offering only those versions, by ALPN or in the moql SETUP, fails the handshake.
Peers must speak moq-transport draft 18 or later, or moq-lite.
The moql SETUP (no ALPN, or `moql`) now negotiates only moq-lite versions.
`session::Limits::requests` is removed: stream credit bounds every peer request, so drop the field and keep `announces`.

### `Path` and `Relative` have no lifetime

There is one owned `Path` type: a short path is stored inline and a long one shares a reference-counted buffer, so cloning stays cheap. Every path is at most `Path::MAX_BYTES`.

- `PathOwned`, `Path<'a>`, and `Path<'static>` are all `Path`; `RelativeOwned` and `Relative<'a>` are `Relative`.
- `Path::{to_owned, into_owned, borrow}` and the `Relative` equivalents are gone: drop the call, or `clone()` a `&Path` you need to keep.
- `Path::new`, `Relative::new`, `Path::join`, and `Path::resolve` return `Result<_, path::Error>`, failing on a path longer than `Path::MAX_BYTES`. `Path::new` copies its argument; use `Path::from_static("room")` for a literal, which borrows it and panics if it is too long.
- `From<&str>`, `From<&String>`, and `From<String>` for `Path` and `Relative` are `TryFrom`, with `path::Error` as the error.
- `AsPath` is gone. Origin entry points such as `publish`, `create_broadcast`, `scope`, `mount`, and `routed` take `impl TryInto<Path, Error: Into<Error>>`, so `&str`, `String`, `Path`, and `&Path` still work and a too-long string fails with `Error::BoundsExceeded`. Other functions that took `impl AsPath`, such as `stats::Tier::new`, take `impl Into<Path>`: pass a `Path` or `&Path`.
- `origin::{Producer, Consumer}::absolute` and `announce::Consumer::absolute` return `Result<Path, Error>`.
- `Path::{has_prefix, strip_prefix, join, relative}` take `&Path`: write `path.has_prefix(&Path::from_static("room"))`.
- Decoding a path longer than `Path::MAX_BYTES` fails with `BoundsExceeded`, like one deeper than `Path::MAX_PARTS`.

### Request handlers are `requests()` ([#6752](https://github.com/moq-dev/moq/pull/6752))

Rename `origin::Producer::dynamic(prefix)`, `broadcast::Producer::dynamic()`, `track::Producer::dynamic()`, and `track::Request::dynamic()` to `requests`.
Their handles `origin::Dynamic`, `broadcast::Dynamic`, and `track::Dynamic` are `origin::Requests`, `broadcast::Requests`, and `track::Requests`; their methods are unchanged.

### Names say one thing ([#6751](https://github.com/moq-dev/moq/pull/6751))

Rename these, which kept their behavior:

| Before | After |
|---|---|
| `Session::bandwidth()` | `Session::allocator()` |
| `Client::with_bandwidth` | `Client::with_allocator` |
| `auth::Auth` | `auth::Credential` |
| `track::Consumer::query()`, `track::Querying` | `track::Consumer::info()`, `track::Resolving` |
| `bandwidth::Reservation::consumer()` | `bandwidth::Reservation::consume()` |
| `bandwidth::Reservation::peek()` | `bandwidth::Reservation::rate()` |
| `stats::Control` | `stats::Message` |
| `stats::Bytes` | `stats::Totals` |
| `transport::Stats`, `transport::poll::Session::stats()` | `transport::Metrics`, `transport::poll::Session::metrics()` |

`group::Demand` reads like `track::Demand`: replace `is_used()` with `wanted()`, and `used()`, `unused()`, `poll_used`, and `poll_unused` with a loop over `wanted_changed()` (or `poll_wanted_changed`), which returns the new value each time it changes and errors once the group closes.
`transport::Metrics` drops `bytes_sent` and `bytes_received`: a transport reports them through the `transport::Wire` it returns from `wire()`, which `session::Stats` now reads.
`transport::StatsUnavailable` is gone: `metrics()` defaults to reporting nothing, so delete an implementation that returned it.

### One lifecycle vocabulary ([#6750](https://github.com/moq-dev/moq/pull/6750))

`closed()` on `group::Producer`, `group::Demand`, `track::Producer`, and `track::Demand` now waits for the outcome and returns `Result<()>`: `Ok(())` once the end settles (a finished group; a track whose declared end was reached with every group below it finished), or the abort error. Match on the result where you awaited `()` or treated `Error::Dropped` as a clean end; `poll_closed` returns `Poll<Result<()>>`.
`group::Producer::abort` after `finish` only releases the cache: readers keep the clean end.
A session closing locally aborts its copies with `Error::Stream(StreamError::Session(SessionError::Cancel))`, so their readers end with that error instead of a clean end.
`track::Producer::end()` is `finished()`.

### An idle reservation reads the share it would get

`bandwidth::Reservation::rate()` and its `consume()` report the slice the track would get back while nothing subscribes to it, instead of `None`; it still claims nothing from the other tracks.
`None` now means only that the connection has no estimate or the reservation is gone.
A sender that relied on `None` to hold its rate while unwatched should check its track's demand instead.

### Exports without a consumer are gone

These had no caller outside tests; replace them as follows.

- `goaway::Consumer::{peek, recv}`: poll it, `goaway.poll(&kio::Waiter::noop())` to peek or `kio::wait(|waiter| goaway.poll(waiter)).await` to wait.
- `bandwidth::Producer::{abort, closed}`: drop the last producer to close it.
- `Version::{names, from_alpn, is_ietf}`: use `Versions::all().iter()` or `Version::from_code`, match `alpn()` yourself, and `!is_lite()`.
- `Server::with_origin(origin)`: call `with_publisher(&origin).with_subscriber(origin)`.
- `stats::Handle::{parent, tier}`, `stats::Registry::exclude`, `stats::Traffic::{is_announced, total_bytes}`, `stats::Tier::is_default`: read the public fields, such as `announces_started > announces_ended` or `tier.label().is_empty()`.
- `frame::Consumer::poll_read_all`: use `read_all`, or poll `poll_read_chunk` until the frame ends.
- `frame::Buffer::{is_full, filled_mut}`: compare `len()` with `capacity()`, and read `filled()`.
- `Path::next_part`: split `as_str()` on the first `/`.
- `setup::Token::CAT`: compare `kind` with `1`.
- `Error::TooManyParameters` and `DecodeError::{ExpectedData, Missing, Unsupported}` were never produced; drop their match arms.

### `Client::with_url` replaces `with_path` and `with_authority`

Pass the URL you dial: `Client::new().with_url(url)` instead of `.with_path(path).with_authority(authority)`.
The client derives what SETUP carries from it and from the transport's `address()`: the path and query when SETUP carries the target, plus the authority for `moqt://`, and nothing when the transport's own URI does.
A `unix://` URL names its target in its `?path=` query.

### `stats::Refusal` is `stats::RefusalKind`, and `time::now` is gone (#6510)

Rename `moq_net::stats::Refusal` to `moq_net::stats::RefusalKind`, so it no longer shares a name with `moq_auth::Refusal`; its variants and `Refusals` are unchanged.
Replace `moq_net::time::now()` with `moq_net::time::Instant::now()`, the one way to read the clock.

### Published names refuse navigation segments

Broadcast creation and route publication now refuse literal `.` and `..` segments, including segments in the producer's root.
Resolve relative references before publishing, or rename broadcasts that used these segments.
Rust reports `Error::NavigationSegment`; the existing protocol error codes carry the refusal to a peer.
Path normalization, relative references, and subscription paths retain their semantics.

### A listing says whether a removed track finished (#6399)

`broadcast::TracksEvent` gains `Finished { name }`: the track left the listing because its producer declared its end. `Removed { name }` now means it left unfinished (dropped or aborted without an end, or its route withdrawn). Handle both where you matched `Removed`.
`broadcast::Lister::finish()`, which ended every reader as if the broadcast ended, is `Lister::end()`. `Lister::finish(name)` now unlists a track as finished, beside `remove(name)` for one that left unfinished.

### `Version::uses_setup_negotiation` is removed ([#6483](https://github.com/moq-dev/moq/pull/6483))

Match the version yourself: lite-01 and lite-02 negotiate the version in SETUP; every other version negotiates it through ALPN.

### Sessions box their transport (#6176)

`Driver` and `server::Handshake` lose their transport type parameter: write `moq_net::Driver` and `moq_net::server::Handshake`.
`Client::connect_lite`, `Server::accept_lite`, and `Server::accept_request_lite` are gone; call `connect`, `accept`, or `accept_request`, which accept every ALPN.
Every session now takes a `transport::poll::Boxable` transport, whose streams must also be `Sync` on native.
A `!Send` transport wraps itself in a type that checks the thread on use, such as `moq_uring::transport::Local`.
`Client::connect` and `Server::accept_request` are plain functions returning `impl Future`, so call them as before with `.await`.

### `track::Producer::largest()` is a getter

`track::Producer::largest(&self)` returns the track's largest location now, `None` before any, instead of streaming each newer one; `track::Producer::poll_largest` is gone.
Replace `poll_largest(&kio::Waiter::noop())` and `largest().await` on a producer with `largest()`.
To wait for each newer one, keep a reader and follow its stream: `let mut reader = producer.consume();`, then `reader.largest().await`.

### Transport failures keep their meaning

Handle `Error::IdleTimeout` separately from `Error::Transport`. A local transport close is session cancellation, not clean end-of-content.
Converting a transport failure to `StreamError` now gives `Unroutable` rather than `Internal`, so retry or re-resolve that route where you classified stream errors.

### Track ends use positions

`track::Producer::finish_at` takes a `track::Position` instead of an exclusive group sequence.
Pass `Position::group(n)` where you passed `n`, or a position within the last group to end after a frame.
`Producer::final_sequence()` is `end()` and returns `Option<Position>`; `Subscriber::finished()` and the ordered reader return `Position` instead of `u64`.

### IETF whole-group FETCH answers name the requested group ([#6199](https://github.com/moq-dev/moq/issues/6199))

On moq-transport drafts 14 through 18, a whole-group `FETCH_OK` now names `{g, 0}` instead of `{g+1, 0}`.
Readers accept both forms and set End Of Track to the sequence after the fetched group.
An older moq-net reader can reject the new answer; upgrade it before pairing it with a new publisher, or use draft 22, whose inclusive End Location is unchanged.
No Rust API changes.

### Receive streams are read through leases (#5878)

`transport::poll::RecvStream` now has `poll_lease(cx, want)`, `set_priority`, and `stop`, plus the provided `poll_read_chunk` and `read_chunk`.
`poll_read`, `poll_read_buf`, `poll_closed`, `read`, `read_buf`, `closed`, `Read`, and `ReadBuf` are gone.
A lease borrows the stream's unreleased bytes as a `bytes::Buf`: reading moves past bytes for good, but they keep their flow-control credit until `Lease::release()`.
Bytes not read stay for the next lease, `Lease::chunks()` peeks without reading, and `is_fin` replaces `closed`.
A transport that can only hand out chunks implements `transport::poll::Chunks` (`poll_read_chunk` and `stop`) and wraps each stream in `transport::poll::Buffered`.

```rust
// Before
let n = stream.read(&mut buf).await?;

// After: copy what arrived, then return its credit
let mut lease = poll_fn(|cx| stream.poll_lease(cx, 1)).await?;
let n = lease.remaining().min(buf.len()); // 0 with `lease.is_fin()` is the end
lease.copy_to_slice(&mut buf[..n]);
lease.release();
```

`transport::CloseKind` gained `Evicted`, which `Error::from_transport` maps to `Error::Evicted`.
`transport::Stats` gained `streams_evicted()`, and `session::Stats` the matching `streams_evicted` field.

### Frame writers own their group (#6111)

`group::Producer::create_frame` now takes the writer by value and returns an owned `frame::Producer` with no lifetime.
`frame::Producer::finish` hands the group writer back, and `frame::Producer::group()` returns `&group::Producer` instead of a `group::Info` copy.
A frame the group refuses (closed, or past its size) now aborts the group.

```rust
// Before
let mut frame = group.create_frame(info)?;
frame.write(payload)?;
frame.finish()?;
// After
let mut frame = group.create_frame(info)?;
frame.write(payload)?;
group = frame.finish()?;
```

A caller that holds the group behind `&mut` can write a whole frame with `write_frame` instead.

### A track's end tolerates a repeat, and skipped groups can be dropped (#6111)

`track::Producer::drop_groups(range, err)` marks groups the publisher skipped, so readers stop waiting for them and a group still arriving there ends with `err`.
`finish_at` now accepts a repeat of the same end and a refinement within the last group; any other second end fails with `Error::ProtocolViolation`.

### A frameless group is not the live edge (#6111)

Every group reaches readers as soon as it is created, but the live edge, `largest()`, and the judgment of whether a group was succeeded count a group only once its first frame opens or it finishes.
A group followed by a frameless one is judged against the next group that has a frame.

### Ending a track aborts its open groups (#6111)

Aborting a track (`abort` or `abort_unused`) now aborts its unfinished groups with the same error, so their readers end with it and their writers fail.
Dropping the last `track::Producer` without a finish aborts them with `Error::Dropped`.

### Priorities are signed, with 0 unset (#6142)

`track::Info::priority`, `track::Subscription::priority`, `group::Fetch::priority`, and `group::Request::priority()` are `i8`, higher first, and 0 is the unset midpoint.
The track's default was 127, the midpoint, and the subscription's was 0, the least urgent; both are 0 now.
To keep a value's moq-lite byte, subtract 128 from it: 255 becomes 127, 128 becomes 0, and 0 becomes -128.
The publisher's `Info::priority` now breaks ties between subscriptions at one subscriber priority.

On the wire, moq-lite carries `priority + 128` and moq-transport `127 - priority`, so an unset priority goes out as 128 on moq-lite (was 127) and 127 on moq-transport (was 128), and an absent moq-transport priority reads as 0.

### A client session asks only about what its readers watch (#6165)

On moq-lite, a client session no longer asks its peer for announcements under its whole subscribe scope at connect.
It asks only under the prefixes its subscribe origin's readers watch, with `announced()`, `follow`, `routed`, `routed_broadcast`, or `announced_broadcast`, and stops asking 2s after nothing at or beneath a prefix is watched.
`request_broadcast` watches nothing, so on a client it fails with `Error::Unroutable` for a path no reader watches, even when the peer announces it.
Resolve a path with `routed_broadcast` or `announced_broadcast`, or hold an `announced()` stream over its prefix while requesting, the way `routed` already did after connecting.
A broadcast resolved through a discovered route keeps its path watched until dropped.
A server session, and a client fed into a `Producer::peer` or `Producer::upstream` handle, still asks for everything it may, so a relay discovers as before.
Serving another session's request is not a watch, so a client that relays what it discovers to its own sessions should feed its client session `origin.upstream()` (or `peer()`) instead of a plain handle.
moq-transport sessions, and moq-lite-03/04 sessions (whose announce streams mark no initial set), are unchanged.

### Frames carry their arrival time

`frame::Frame` gains a private arrival stamp, read with `Frame::arrived()`, so it can no longer be built with a struct literal.
Replace `Frame { timestamp, payload }` with `Frame::new(timestamp, payload)`.
`timestamp` and `payload` stay public fields.

### Timestamps stop at 2^61 - 1 (#6168)

`Timestamp::new`, its `from_*` constructors, `convert`, `checked_add`, and `TryFrom<Duration>` return `TimeOverflow` past `2^61 - 1` instead of `2^62 - 1`, the most lite-05/06 can carry as a group's first frame.
A received timestamp past it fails the group as a malformed track.
Clamp timestamps you build from unbounded input to `(1 << 61) - 1`.

### A dropped or cut group ends its reader with an error (#6143)

A group the publisher cut, or named in `SUBSCRIBE_DROP`, after a `track::Subscriber` saw it is now handed out, and reading it fails with the publisher's error; before, it was skipped or ended cleanly.
A group already held fails the same way when the drop lands, even while its stream is still arriving.
On lite-05 and lite-06 the error is always `Error::Stream(StreamError::Old)`.
Treat a failed group read as the loss of that group and keep reading the track, rather than ending the subscription.

### A claim answers by publishing (#5678)

`origin::Producer::dynamic(prefix)` takes no route: an application claim has no epoch, cost, or hops.
Answer each `origin::Request` with `request.prepare(epoch)?` (a fresh `Epoch::mint()` per output run), fill `prepared.broadcast()`, then `prepared.publish()?`, which announces the broadcast at the exact path under that epoch and returns its producer.
Only `publish` announces it: `prepared.broadcast().announce(..)` fails with `Error::Closed`.
`Dynamic::update` is gone: drop the claim and claim again to change it, and close an answer's producer to end that output.
`Request::accept`, `Producer::dynamic_route`, and `Dynamic::update_route` are crate-private (#6496): only the built-in sessions answer with a broadcast they already hold.
Dropping a `Prepared` unpublished refuses the request, as dropping a `Request` does, and `publish` fails with `Error::Unroutable` once the claim went away.

### Anchors can name a time (#6074)

`track::Anchor` gains `Time(Timestamp)`, the group containing a presentation time, so exhaustive matches need an arm for it.
Subscribing or updating with one on an untimed track fails with `Error::TimestampMismatch`.

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
Await `largest()` and read `.group` where you used the sequence; on a `Producer`, `largest()` reads the edge without waiting.
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
A group received over moq-lite-07 is checked the same way.
A track received over moq-lite-05/06 keeps its timestamps unchecked, since they may be an untimed origin's send times, and so does one received over moq-transport.
A relay serves both on over every version, where a moq-lite-07 receiver checks them and may refuse a group that rewinds.
A publisher whose timeline restarts, such as an HLS `EXT-X-DISCONTINUITY` that resets decode times, publishes a new broadcast instead.

### Broadcast `finish` is gone

`broadcast::Producer::finish()`, a deprecated alias for `close()`, is removed: a broadcast has no finished state.
Call `close()` where you called `finish()`.
To end content cleanly, finish each track, keep the producer for as long as late readers should find it, then close it; `moq_mux::Linger` does this for a hang broadcast.

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
A custom transport implements these traits and reports `address()`: `transport::Address::Uri` when its handshake carries the request URI, or `Setup` when SETUP carries it.

```rust
impl moq_net::transport::poll::Session for MySession {
	fn address(&self) -> moq_net::transport::Address {
		moq_net::transport::Address::Uri
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

A peer past the session's announce bound loses the session with `TooManyRequests`.
Stream credit bounds its subscriptions and FETCHes.
Raise the defaults for a large relay mesh.

```rust
let mut limits = session::Limits::default();
limits.announces = 1_000_000;
let client = Client::new().with_limits(limits);
```

### A path is one broadcast (#4741)

Broadcast identity is the path, whatever the publisher or first hop.
A publisher that restarts its group numbering must publish under a new broadcast name.

### Broadcast `abort` is removed (#4152)

`broadcast::Producer::abort` is removed; use `close()` for an immediate end without a cause.
`broadcast::Consumer::is_finished` is gone, and `closed()` on `Consumer` and `Dynamic` returns `()`; the broadcast end carries no cause.

### No `VarInt`

Varints are plain `u64`s.
Use `moq_net::varint::decode_quic(buf)` and `moq_net::varint::encode_quic(v, buf)`, which fails past `2^62 - 1`.

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
