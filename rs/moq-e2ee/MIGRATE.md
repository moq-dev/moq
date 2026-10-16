# Migrating moq-e2ee

How to update your code for each breaking change, newest first.

## Unreleased

### The epoch comes from the route (#4942)

The protected path is the opaque broadcast path; the publisher epoch is read from the route instead of a path suffix.
On wires that do not carry an epoch, the application supplies it.

### Timestamps are optional

`Frame` and `Datagram` timestamps are `Option<Timestamp>`, and the writers take `impl Into<Option<Timestamp>>`, so passing a `Timestamp` still compiles.
A frame read from an untimed track has no timestamp.

```rust
// Before
let ts: Timestamp = frame.timestamp;
// After
let ts: Option<Timestamp> = frame.timestamp;
```
