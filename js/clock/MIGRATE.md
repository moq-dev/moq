# Migrating @moq/clock

How to update your code for each breaking change, newest first.

## Unreleased

### Manual time steps through timer deadlines

Rename `await clock.advanceAsync(duration)` to `await clock.advance(duration)`.
The async method now visits every due deadline and drains tasks before continuing,
so timers that re-arm during the advance can fire again before its target.

Rename synchronous `clock.advance(duration)` calls to `clock.jump(duration)`.
Use `jump` to model a stalled event loop, firing due timers late at the target;
use awaited `advance` for normal time progression. There are no compatibility aliases.

Drive each `Manual` serially. Until `advance` or `fire` settles, another
`advance` or `fire` rejects and `jump` throws without moving time. Timer callbacks
may arm timers, but must leave driving that clock to their caller.
