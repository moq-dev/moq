# Migrating @moq/json

How to update your code for each breaking change, newest first.

## Unreleased

### Compression moves to the track

The `Compression` type, every `compression` option, `Desync`, `Stream.Decoder.reset`, and `Window.DecoderConfig` are gone, along with the `@moq/flate` dependency.
Frames are plain JSON: create the track with `compress: true` instead, and `@moq/net` compresses each group on the wire and hands every reader plain frames.
Drop `compression` from producer and consumer options, and construct `Window.Decoder` with no arguments. A consumer reads a compressed track with no option of its own.
Every frame is charged its JSON size against `Group.MAX_GROUP_CACHE_BYTES`, as a compressed one already was.

### Groups roll on joiner demand

`Snapshot` `deltaRatio` and `Window` `opRatio` are replaced by `deltas` and `ops` booleans (both default `true`).
Pass `deltas: false` where you passed `deltaRatio: 0`, and `ops: false` where you passed `opRatio: 0`.
A group no longer rolls on a byte budget: create the track with a `refresh` ratio (`8` matches the old default), and the producer restates the value or window in a new group when a joiner asks.
Without a ratio a group rolls only at its frame and byte caps.
An `Encoder` user restates with `encoder.snapshot()` once `demand().wanted` passes the largest group it wrote.

`encoder.value` (and so `producer.mutate()`) now holds the last committed value: a rejected publish no longer carries into the next edit, so retry the edit yourself if you still want it.

### Duplicate keys are refused

Every decoder throws `DuplicateKey` for a frame in which a JSON object repeats a key, where `JSON.parse` used to keep the last copy.
Fix any publisher that writes a repeated key.

### Consumers return `Timed` values (#5099)

The snapshot, stream, and window consumers' `next()` and async iterator yield `{ value, at }`, where `at` is the frame's timestamp and absent on an untimed track.
A window event's `value` is the `Event` that `next()` used to return; a record restated by a group header carries the header frame's time.
A snapshot consumer's `next()` yields every state in order; call `latest()` to skip to the newest, as `next()` used to.
Where you awaited `next()` for the newest value, read `(await consumer.latest())?.value`.
A caller that passes the result on as `any` or `unknown` still compiles, so search for every `next()` and `for await` rather than relying on type errors.

### Producers never invent a timestamp (#5070)

`Snapshot.Producer.update`, `Stream.Producer.append`, and `Window.Producer.push` take a `Timed<T>`, and `Window.Producer.pop` and `Snapshot.Producer.mutate` take an optional `at`.
Pass `{ value, at: Time.Timestamp.now() }` instead of the value.
Omit `at` only on an untimed track.

## 0.3 -> 0.4

### One options object (#3640)

Constructors take one options object.
Spread the config beside the track: `new Json.Snapshot.Consumer({ track, ...config })`.
