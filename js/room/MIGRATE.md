# Migrating @moq/room

How to update your code for each breaking change, newest first.

## Unreleased

### `HISTORY` is a `Nano` (#5706)

`HISTORY` is a `Nano` from `@moq/clock` instead of milliseconds.

```ts
// Before
const ms = HISTORY;
// After
const ms = Milli.fromNano(HISTORY);
```

### Chat tracks are timed (#4968)

`Chat.info()` includes a `timescale`, so a chat track you create yourself must declare one before writing timestamps.
