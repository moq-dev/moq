# Migrating @moq/loc

How to update your code for each breaking change, newest first.

## Unreleased

### Timestamps are `@moq/clock` units (#5706)

Frame timestamps are typed as `Micro` from `@moq/clock` instead of `Time.Micro` from `@moq/net`.
Import the `Micro` type from `@moq/clock`.
