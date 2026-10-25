[![Documentation](https://docs.rs/moq-archive/badge.svg)](https://docs.rs/moq-archive/)
[![Crates.io](https://img.shields.io/crates/v/moq-archive.svg)](https://crates.io/crates/moq-archive)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-archive

MoQ broadcast archives on any
[`object_store::ObjectStore`](https://docs.rs/object_store).

`Producer` records every track of one broadcast under a key prefix, with no
catalog dependency: it lists the tracks with TRACKS and subscribes to each,
storing duration-cut objects keyed by group, frame, and timestamp. A lost
group becomes a gap instead of a stall, a restarted producer resumes each
track where it stopped, and an optional DVR window prunes old objects. The
host picks the prefix and runs one producer per broadcast.

`Consumer` serves a stored prefix back as an ordinary broadcast: every track,
the archived catalog included, with its original groups, frames, timescale,
and priority. Each track is live from its newest stored group, older groups
are fetched from the store on request, and a growing archive is followed until
its producer marks it complete.

```bash
cargo add moq-archive
```

```rust
use moq_archive::object_store::memory::InMemory;

// `broadcast` is a `moq_net::broadcast::Consumer`.
let store = moq_archive::Store::new(InMemory::new(), "recordings/demo");
moq_archive::Producer::new(store.clone(), broadcast, Default::default()).run().await?;

// `served` is an `origin::Publication`'s fill, announced once `open` returns.
let consumer = moq_archive::Consumer::open(store, served, Default::default()).await?;
consumer.run().await?;
```

See [docs.rs/moq-archive](https://docs.rs/moq-archive) for the object layout.
