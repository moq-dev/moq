# Migrating moq-msf

How to update your code for each breaking change, newest first.

## Unreleased

### `Track::stalled` is removed (#4915)

The non-standard `stalled` track field is gone and no longer written.
Drop it from any `Track` you build.
