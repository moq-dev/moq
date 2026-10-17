# Migrating moq-e2ee

How to update your code for each breaking change, newest first.

## Unreleased

### The epoch comes from the route (#4942)

The protected path is the opaque broadcast path; the publisher epoch is read from the route instead of a path suffix.
On wires that do not carry an epoch, the application supplies it.

```rust
// Before
let path = cred.path(name)?.join_epoch(Some(generation.epoch()));
let (_, epoch) = path.split_epoch();
// After
let path = cred.path(name)?; // announce with Route::default().with_epoch(epoch)
let epoch = consumer.routed(&path).await.and_then(|route| route.epoch);
```

### Timestamps are optional

`Frame` and `Datagram` timestamps are `Option<Timestamp>`, and the writers take `impl Into<Option<Timestamp>>`, so passing a `Timestamp` still compiles.
A frame read from an untimed track has no timestamp.

```rust
// Before
let ts: Timestamp = frame.timestamp;
// After
let ts: Option<Timestamp> = frame.timestamp;
```
