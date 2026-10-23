# Migrating moq-msf

How to update your code for each breaking change, newest first.

## Unreleased

### `Catalog` parses through `FromStr` (#6471)

The inherent `Catalog::from_str` is replaced by `impl FromStr for Catalog`, with the same `serde_json::Error`.
Call `json.parse::<Catalog>()`, or bring `std::str::FromStr` into scope to keep calling `Catalog::from_str`.

### `Track::stalled` is removed (#4915)

The non-standard `stalled` track field is gone and no longer written.
Drop it from any `Track` you build.
