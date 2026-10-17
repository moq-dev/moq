# Migrating @moq/msf

How to update your code for each breaking change, newest first.

## Unreleased

### `Track.stalled` is removed (#4915)

The non-standard `stalled` track field is gone and no longer read or written.
Drop it from any track you build.

```ts
// Before
const track: Track = { name: "video0", packaging: "loc", stalled: true };
// After
const track: Track = { name: "video0", packaging: "loc" };
```
