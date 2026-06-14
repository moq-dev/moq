<p align="center">
	<img height="128px" src="https://github.com/moq-dev/moq/blob/main/.github/logo.svg" alt="Media over QUIC">
</p>

# moq-data

[![crates.io](https://img.shields.io/crates/v/moq-data)](https://crates.io/crates/moq-data)
[![docs.rs](https://img.shields.io/docsrs/moq-data)](https://docs.rs/moq-data)

Helpers for sending metadata over [Media over QUIC](https://moq.dev/) tracks.

Each helper maps an application data structure onto a [`moq-net`](../moq-net) track, handling snapshots and deltas so a late joiner can reconstruct the current state from the newest group alone.

- **`set`** syncs a `HashSet`-like collection of arbitrary binary items, encoding changes as `+`/`-` deltas.
- **`json`** re-exports [`moq-json`](../moq-json) for snapshot/delta JSON publishing. It lives in its own crate today and will migrate here over time.

## Set

```rust
use moq_data::set;

// Publish the set of track names in a broadcast.
let mut tracks = set::Producer::<String>::new(track, set::Config::default());
tracks.insert("video".to_string())?;
tracks.insert("audio".to_string())?;
tracks.remove("audio")?;

// Consume: yields the full set after each change.
let mut consumer = set::Consumer::<String>::new(track.subscribe(None));
while let Some(names) = consumer.next().await? {
	println!("{names:?}");
}
```

Each group is self-contained: its first frame is a full snapshot of every item and any following frames are single `+` (insert) or `-` (remove) deltas applied in order. A consumer jumps to the newest group, reads the snapshot, and replays the deltas.

Items are arbitrary binary data: implement the `set::Item` trait for any type. It encodes straight into the frame's `bytes::BufMut` (one copy, no intermediate buffer) and decodes from a `bytes::Bytes` slice. `String`, `Vec<u8>`, and `bytes::Bytes` are supported out of the box.

Deltas are on by default (`Config { delta_ratio: Some(2.0) }`); a delta is appended while the group stays within `delta_ratio` times the size of a fresh snapshot, otherwise a new snapshot group is started. Set `delta_ratio: None` to publish a full snapshot per change.

## Features

| Feature | Default | Description |
|---|---|---|
| `set` | yes | The `HashSet`-like collection. |
| `json` | yes | Re-export of `moq-json`. |
