# Migrating moq-e2ee

How to update your code for each breaking change, newest first.

## Unreleased

### Physical names are reversible and variable length (#6665)

`Name` is no longer `Copy` or 22 characters: it is the semantic name padded to a multiple of 16 bytes and encrypted under the generation, at least 59 characters.
Clone a `Name` where it was copied, and size nothing by its old length.
Physical names and payload keys under `moq-e2ee-00` change, so publishers and subscribers must upgrade together.

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

The protected path is the opaque broadcast path; the publisher epoch is read from the route instead of a path suffix.
On wires that do not carry an epoch, the application supplies it.
Drop `join_epoch` and `split_epoch`: announce with `Route::default().with_epoch(epoch)`, and read it back with `consumer.routed(&path).await.and_then(|route| route.epoch)`.

### Timestamps are optional

`Frame` and `Datagram` timestamps are `Option<Timestamp>`, and the writers take `impl Into<Option<Timestamp>>`, so passing a `Timestamp` still compiles.
A frame read from an untimed track has no timestamp.
Handle `None` where you read `frame.timestamp`.
