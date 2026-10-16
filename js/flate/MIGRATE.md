# Migrating @moq/flate

How to update your code for each breaking change, newest first.

## Unreleased

### @moq/binary is @moq/flate

The opaque `Snapshot` and `Stream` tracks moved beside the codec; the wire and the catalog's `binary` section are unchanged.

```ts
// Before
import { Snapshot, Stream } from "@moq/binary";
// After
import { Snapshot, Stream } from "@moq/flate";
```

### Consumers return `Timed` values (#5099)

The snapshot and stream consumers' `next()` and async iterator yield `{ value, at }`, where `at` is absent on an untimed track.
A snapshot consumer's `next()` yields every state in order; call `latest()` to skip to the newest.

```ts
// Before
const bytes = await consumer.next();
// After
const bytes = (await consumer.latest())?.value;
```

### Producers never invent a timestamp (#5070)

`Snapshot.Producer.update` and `Stream.Producer.append` take a `Timed<Uint8Array>`.
Omit `at` only on an untimed track.

```ts
// Before
producer.update(bytes);
// After
producer.update({ value: bytes, at: Time.Timestamp.now() });
```
