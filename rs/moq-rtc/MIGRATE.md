# Migrating moq-rtc

How to update your code for each breaking change, newest first.

## Unreleased

### Sessions return their UDP wire source (#5632)

`Client::publish` and `Client::subscribe` return `Result<wire::Source>` instead of `Result<()>`.
Read per-peer byte counts with `source.stats()`, or discard it.

```rust
// Before
client.subscribe(url, broadcast).await?;
// After
let _source = client.subscribe(url, broadcast).await?;
```

### `tick` is removed (#4915)

`MediaSink::tick` and `Bridge::tick` are gone; a stalled rendition is marked by the catalog's `enabled` flag.
Delete the calls.
