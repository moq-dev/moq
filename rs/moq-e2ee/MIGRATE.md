# Migrating moq-e2ee

How to update your code for each breaking change, newest first.

## Unreleased

### Datagrams are their own track kind ([#7752](https://github.com/moq-dev/moq/pull/7752))

`track::Producer::{append_datagram, insert_datagram}` and `track::Consumer::{poll_recv_datagram, recv_datagram}` are removed. Create a datagram track with `broadcast::Producer::create_datagrams(name, info)` and read it with `broadcast::Consumer::datagrams(name)`, which return `datagram::Producer` (`append`, `insert`) and `datagram::Consumer` (`recv`, yielding the same `datagram::Event`). Its sequence is its own, not the stream track's group counter, and its key domain stays separate.

### Tracks are named semantically through `broadcast` wrappers (#5876)

`generation::Producer::produce` and `generation::Consumer::consume` are private.
Wrap the broadcast in a `broadcast::Producer` or `broadcast::Consumer` and name tracks by their semantic names; the wrapper derives the physical name.

```rust
// Before
let name = generation.name("video")?;
let video = generation.produce(broadcast.create_track(name.as_str(), info)?)?;
let subscriber = remote.track(name.as_str())?.subscribe(subscription).await?;
let reader = consuming.consume(subscriber)?;

// After
let video = moq_e2ee::broadcast::Producer::new(broadcast, generation).create_track("video", info)?;
let reader = moq_e2ee::broadcast::Consumer::new(remote, consuming)
	.subscribe("video", subscription)?
	.await?;
```

A track served on demand is reserved up front with `broadcast::Producer::reserve_track` and accepted once its `demand()` shows a subscriber.

### The epoch comes from the route (#4942)

The protected path is the opaque broadcast path; the publisher epoch is read from the route's instance instead of a path suffix.
On wires that do not carry an instance, the session mints one, so the application supplies the epoch.
Drop `join_epoch` and `split_epoch`: announce the generation's `moq_e2ee::Epoch` as the route's instance with `Route::default().with_instance(generation.epoch().instance(), 0)`, and read it back from the resolved broadcast with `moq_e2ee::Epoch::of(&broadcast)`.

### Timestamps are optional

`Frame` and `Datagram` timestamps are `Option<Timestamp>`, and the writers take `impl Into<Option<Timestamp>>`, so passing a `Timestamp` still compiles.
A frame read from an untimed track has no timestamp.
Handle `None` where you read `frame.timestamp`.
