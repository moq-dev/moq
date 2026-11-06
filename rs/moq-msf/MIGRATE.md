# Migrating moq-msf

How to update your code for each breaking change, newest first.

## Unreleased

### `Track::init_data` shares referenced payloads

`Track::init_data` is now `Option<Arc<str>>` instead of `Option<String>`.
Convert owned strings with `.into()` when assigning the field. Use `.as_deref()`
to read it as `Option<&str>`. Tracks resolving the same `initRef` share one allocation.

### `Catalog` parses through `FromStr` (#6471)

The inherent `Catalog::from_str` is replaced by `impl FromStr for Catalog`, with the same `serde_json::Error`.
Call `json.parse::<Catalog>()`, or bring `std::str::FromStr` into scope to keep calling `Catalog::from_str`.

### `Track::stalled` is removed (#4915)

The non-standard `stalled` track field is gone and no longer written.
Drop it from any `Track` you build.
