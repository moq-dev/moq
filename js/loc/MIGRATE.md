# Migrating @moq/loc

How to update your code for each breaking change, newest first.

## Unreleased

### `Producer` is removed

`Producer` and `Source` are gone; the package decodes LOC only.
Publish media through `@moq/publish`, or write LOC frames yourself: a varint property length, the `0x10` timestamp property in microseconds, then the codec payload.

### Timestamps are `@moq/clock` units (#5706)

Frame timestamps are typed as `Micro` from `@moq/clock` instead of `Time.Micro` from `@moq/net`.
Import the `Micro` type from `@moq/clock`.
