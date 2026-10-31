# Migrating @moq/room

How to update your code for each breaking change, newest first.

## Unreleased

### `claims` refuses an identity with an empty segment ([#6648](https://github.com/moq-dev/moq/pull/6648))

`claims(room, identity)` throws on an identity with a leading, trailing, or doubled `/` instead of collapsing it: pass `alice`, not `alice/`.

### Chat events carry their send time

`Chat.Subscriber.recv()` returns `{ value, at }`: `value` is the event `recv()` used to return, and `at` is its frame's timestamp.
Read `(await subscriber.recv())?.value` where you used the event directly.

### `HISTORY` is a `Nano` (#5706)

`HISTORY` is a `Nano` from `@moq/clock` instead of milliseconds.
Use `Milli.fromNano(HISTORY)` where you need milliseconds.

### Chat tracks are timed (#4968)

`Chat.info()` includes a `timescale`, so a chat track you create yourself must declare one before writing timestamps.
Create it with `broadcast.createTrack(Chat.TRACK, Chat.info())`.
