# Migrating moq-net

How to update your code for each breaking change, newest first.

## Unreleased

### Track readers follow `largest()` instead of reading `latest()` (#5725)

`latest()` on `track::{Producer, Consumer, Subscriber, Ordered}` is gone.
`largest()` streams the newest frame's location: the current one at once, then each newer one, and `None` once the track ends.
It can name a frame the publisher reported before its group arrives, and an empty group is not reported.
Poll it once with a no-op waiter where you read the edge without waiting.

```rust
// Before
let edge = subscriber.latest(); // Option<u64>
// After
let edge = subscriber.largest().await.map(|position| position.group);
// Or, without waiting
let edge = match subscriber.poll_largest(&kio::Waiter::noop()) {
	Poll::Ready(Some(position)) => Some(position.group),
	_ => None,
};
```

### A timed track refuses a group that rewinds (#5803)

A local write whose first frame starts before a lower-sequence group's, or after a higher one's, fails with `TimestampMismatch`; `@moq/net` throws it the same way.
Received groups are accepted as is, since their timestamps may be send times.
A publisher whose timeline restarts, such as an HLS `EXT-X-DISCONTINUITY` that resets decode times, publishes a new broadcast instead.

### A bare track handle is not demand (#5820)

Holding a `track::Consumer` sends nothing and no longer keeps the producer used.
Only a query, subscription, or fetch made from it counts, until it is dropped.
Hold one of those for as long as you want the track.

```rust
// Before
let track = broadcast.track("video")?; // kept the producer used
// After
let track = broadcast.track("video")?;
let _sub = track.subscribe(None); // or `track.query()` to only learn its info
```

### Dynamic broadcasts see requests when they are made (#5820)

`broadcast::Consumer::track` no longer enqueues a request.
`broadcast::Producer::requested_track` yields it once the handle queries, subscribes, or fetches.
