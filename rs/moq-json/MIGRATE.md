# Migrating moq-json

How to update your code for each breaking change, newest first.

## Unreleased

### A compressed group is held to the group budget

A compressed group may inflate to at most `moq_net::group::MAX_CACHE_BYTES` (32 MiB), the most a plain group carries, so a peer's small frames cannot make a reader hold more than an uncompressed track would.
The encoders charge each frame its plaintext: a compressed snapshot or window header over 32 MiB fails with `moq_net::Error::FrameTooLarge` (snapshot) or the window's group cache error, where it used to pass up to 64 MiB, and deltas and ops roll a new group once a group's plaintext would pass the budget.
A compressed stream's budget is 32 MiB of record plaintext.
`snapshot::consumer::Config::max_size` still sets a tighter limit, but no longer stops inflation early; a frame inflates within the group budget and is then refused.

### Groups roll on joiner demand

`snapshot::Config::delta_ratio` and `window::ProducerConfig::op_ratio`, with `with_delta_ratio` and `with_op_ratio`, are replaced by `deltas: bool` and `ops: bool` (both default `true`).
Set `deltas: false` where you set `delta_ratio = 0`, and `ops: false` where you set `op_ratio = 0`.
A group no longer rolls on a byte budget: create the track with `Info::refresh` (`Some(8)` matches the old default) and drive `Producer::refresh` (or `poll_refresh`) beside your edits, so a joiner past the ratio gets the value or window restated in a new group.
Without a ratio a group rolls only at its frame and byte caps.
An `Encoder` user restates with `Encoder::snapshot()` once `Demand::wanted` passes the largest group it wrote.

`Encoder::value` (and so `Producer::modify`/`mutate`) now holds the last committed value: a rejected publish no longer carries into the next edit, so retry the edit yourself if you still want it.

### Consumers return `Timed<T>` (#5677)

Snapshot, stream, and window `next` / `poll_next` return `moq_net::Timed<T>`: read `.value` for the value and `.at` for the frame's exact timestamp, `None` when untimed.
A window event's `.value` is the `Event` that `next` used to return; a record restated by a group header carries the header frame's time.
Snapshot `next` now yields every state in order inside the subscription's `max_delay`; call `latest` / `poll_latest` for the old collapse-to-newest read.

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
