# Migrating moq-e2ee

How to update your code for each breaking change, newest first.

## Unreleased

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
