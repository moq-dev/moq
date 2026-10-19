# Migrating moq-json

How to update your code for each breaking change, newest first.

## Unreleased

### Duplicate keys are refused

Every decoder refuses a frame in which a JSON object repeats a key with `Error::DuplicateKey`, where stream and window decoders used to keep the last copy and snapshots returned `Error::Json`.
Match `DuplicateKey` wherever you handle `Error::Json` for a malformed frame, and fix any publisher that writes a repeated key.

### Demand is read through `demand()`

Call `producer.demand().is_used()` instead of `producer.is_used()`.

### Timestamps are explicit

`moq_net::Timed<P>` drops its clock type parameter.
Snapshot, stream, and window producers publish `at: None` untimed, so their track must be untimed; pass a timestamp on a timed track.
Snapshot `modify(at)` and `mutate(at, f)` carry the caller's optional time through the edit guard.
Wrap each update as `Timed::from(&value).at(timestamp)`, and pass `Some(timestamp)` to `modify`.

## 0.3 -> 0.4

### Configs split by role (#3718)

`compression: bool` is a `Compression` enum, and options that own a track live in each mode's `producer::Config` and `consumer::Config`.
`lock()` is `modify()?`, and a guard that fails to publish on drop aborts the track (#3644).
Set `Compression::Deflate` where you set `true`.
