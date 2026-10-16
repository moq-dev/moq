# Migrating moq-json

How to update your code for each breaking change, newest first.

## Unreleased

### Demand is read through `demand()`

```rust
// Before
if producer.is_used() {}
// After
if producer.demand().is_used() {}
```

### Timestamps are explicit

`moq_net::Timed<P>` drops its clock type parameter.
Snapshot, stream, and window producers publish `at: None` untimed, so their track must be untimed; pass a timestamp on a timed track.
Snapshot `modify(at)` and `mutate(at, f)` carry the caller's optional time through the edit guard.

```rust
// Before
producer.update(&value)?;
let mut guard = producer.modify()?;
// After
producer.update(Timed::from(&value).at(timestamp))?;
let mut guard = producer.modify(Some(timestamp))?;
```

## 0.3 -> 0.4

### Configs split by role (#3718)

`compression: bool` is a `Compression` enum, and options that own a track live in each mode's `producer::Config` and `consumer::Config`.
`lock()` is `modify()?`, and a guard that fails to publish on drop aborts the track (#3644).

```rust
// Before
config.compression = true;
let mut value = producer.lock();
// After
config.compression = Compression::Deflate;
let mut value = producer.modify()?;
```
