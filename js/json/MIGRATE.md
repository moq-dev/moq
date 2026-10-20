# Migrating @moq/json

How to update your code for each breaking change, newest first.

## Unreleased

### Duplicate keys are refused

Every decoder throws `DuplicateKey` for a frame in which a JSON object repeats a key, where `JSON.parse` used to keep the last copy.
Fix any publisher that writes a repeated key.

### Consumers return `Timed` values (#5099)

The snapshot and stream consumers' `next()` and async iterator yield `{ value, at }`, where `at` is the frame's timestamp and absent on an untimed track.
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
