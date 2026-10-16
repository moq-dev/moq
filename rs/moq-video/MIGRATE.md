# Migrating moq-video

How to update your code for each breaking change, newest first.

## Unreleased

### Capture takes one `encode::Capture` and returns a `Control`

`publish_capture(broadcast, catalog, capture, encode, clock)` is `publish_capture(broadcast, catalog, encode::Capture)`.
`Capture` has no clock: the driver stamps on `catalog.clock()`.
`encode::Control::new` returns the control handle and the driver; dropping the last `Control` cancels an in-flight open or encode.

```rust
// Before
publish_capture(broadcast, catalog, capture, encode, clock).await?;
// After
let mut options = encode::Capture::default();
options.capture = capture;
options.encode = encode;
publish_capture(broadcast, catalog, options).await?;
```

### `Control::cut` returns a `Result`

`cut()` fails with `Error::CutUnsupported` once the driver knows its backend cannot force a keyframe, and the publish carries on at its cadence.
It counts cadence keyframes too, so a request may be served without forcing one.

```rust
// Before
control.cut();
// After
if let Err(Error::CutUnsupported) = control.cut() {
	// wait for the next cadence keyframe
}
```

### `Encoded` marks keyframes (#4295)

`encode::Encoded::new` takes a third `keyframe: bool`, also readable as `Encoded::keyframe`.

```rust
// Before
let encoded = Encoded::new(payload, timestamp);
// After
let encoded = Encoded::new(payload, timestamp, keyframe);
```

### Producers drop `tick`, `idle`, and `observe_lag` (#4915)

`encode::Producer` no longer takes liveness hints; disable a rendition through the catalog's `enabled` flag instead.

### External Vulkan and DMA-BUF inputs (#4975)

On Linux, `Surface::Vulkan` no longer requires the `nvidia` feature.
`vulkan::Slot::new` takes exported handles, an `Image`, and a producer guard, replacing the CUDA-owned producer API.
`vulkan::Importer`, `vulkan::ImportError`, `cuda::Converter`, and `cuda::Slot` leave the public API; `Surface::Cuda` stays for NVDEC output.
`DmaBuf::new` takes an owned FD, a `DmaBufLayout`, and a release guard, and an external DMA-BUF refuses CPU readback.
Bind eager open to an external device with `encode::Config::input`; an unsupported device is refused instead of falling back to software.

### Decoder staleness is `max_delay`

`decode::Options::max_age` is `max_delay`.

```rust
// Before
options.max_age = delay;
// After
options.max_delay = delay;
```
