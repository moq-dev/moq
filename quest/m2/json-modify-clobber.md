# [XS] A snapshot edit never seeds from a default

## Goal

`moq_json::snapshot::Producer::modify` fails when the last published value
does not deserialize as `T` instead of seeding `T::default()` and
publishing a value with every other field dropped, the clobber the API's
own doc says it prevents.

## Plan

`serde_json::from_value(last.clone())?` in `rs/moq-json/src/snapshot/producer.rs`
with a regression that publishes one shape, modifies as another, and
expects an error. Public API: none. Wire: none.

## Required

- [Merge dev](/quest/m1/merge-dev.md) - the dev-only guard this fixes

## Related

- [JSON mutate](/quest/m2/json-mutate.md) - the closure form beside the guard
