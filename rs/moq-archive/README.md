[![Documentation](https://docs.rs/moq-archive/badge.svg)](https://docs.rs/moq-archive/)
[![Crates.io](https://img.shields.io/crates/v/moq-archive.svg)](https://crates.io/crates/moq-archive)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](https://github.com/moq-dev/moq/blob/main/LICENSE-MIT)

# moq-archive

Versioned [hang](https://docs.rs/hang) recordings on any
[`object_store::ObjectStore`](https://docs.rs/object_store).

The crate owns the portable layout and codecs: percent-encoded track names,
per-track `.info` JSON, the binary segment envelope, and put/get/list/delete.
`Store` wraps a store under a recording prefix and stays generic, so a caller
that needs runtime dispatch passes `Arc<dyn ObjectStore>`. A broadcast
advertises its recording through the catalog's
[`archive`](https://doc.moq.dev/concept/hang) entry.

```bash
cargo add moq-archive
```

Group bounds are finite inclusive ranges in first-to-last order:

```rust
let key = moq_archive::Key::groups("video", 5..=7)?;
assert_eq!(key.track(), "video");
```

See [docs.rs/moq-archive](https://docs.rs/moq-archive) for the object layout.
