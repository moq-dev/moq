# Migrating moq-flate

How to update your code for each breaking change, newest first.

## Unreleased

### Consumers return `Timed<T>` (#5677)

Snapshot and stream `next` / `poll_next` return `moq_net::Timed<T>`: read `.value` for the payload and `.at` for the frame's exact timestamp, `None` when untimed.
Snapshot `next` now yields every state in order inside the subscription's `max_delay`; call `latest` / `poll_latest` for the old collapse-to-newest read.

### moq-binary is moq-flate

The opaque `snapshot` and `stream` tracks moved beside the codec, so `moq_binary::X` is `moq_flate::X`; the wire is unchanged.
`moq_binary::Error::Flate(e)` is the matching `moq_flate::Error` variant (`Decompress`, `TooLarge`).
`moq_flate::Error` carries `moq_net::Error`, so it is no longer `PartialEq`.

### Demand is read through `demand()`

Call `producer.demand().is_used()` instead of `producer.is_used()`.

### Timestamps are explicit

Snapshot and stream producers publish `at: None` untimed, so their track must be untimed; pass a timestamp on a timed track.
Snapshot `modify(at)` and `mutate(at, f)` carry the caller's optional time.
Wrap each update as `Timed::from(bytes).at(timestamp)`.
