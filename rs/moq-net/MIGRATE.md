# Migrating moq-net

How to update your code for each breaking change, newest first.

## Unreleased

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
