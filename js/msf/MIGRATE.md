# Migrating @moq/msf

How to update your code for each breaking change, newest first.

## Unreleased

### `CatalogSchema` keeps prototype-named extension members

`CatalogSchema.parse` preserves own `__proto__`, `constructor`, and `toString` members in `ext`, matching `decode`.
The parsed extension map now has a null prototype; inspect it with `Object.hasOwn` and `Object.entries`.

### `Track.stalled` is removed (#4915)

The non-standard `stalled` track field is gone and no longer read or written.
Drop it from any track you build.
