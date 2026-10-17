# Migrating moq-video

How to update your code for each breaking change, newest first.

## Unreleased

### `Color` is a struct of matrix, range, primaries and transfer

The four variants become constants plus a range, and the new `color` module holds `Matrix`, `Range`, `Primaries` and `Transfer`.
`Bt601Limited` is `Color::BT601`, `Bt709Limited` is `Color::BT709`, and the `Full` variants set `range: Range::Full`.
Match on the field you need instead of the variant.
BT.601 now names its primaries (SMPTE 170M), and BT.2020 with PQ or HLG is representable.

```rust
// Before
let color = Color::Bt709Full;
let hd = matches!(color, Color::Bt709Limited | Color::Bt709Full);
// After
use moq_video::color::{Matrix, Range};
let color = Color { range: Range::Full, ..Color::BT709 };
let hd = color.matrix == Matrix::Bt709;
```

### `Encoder::encode` refuses a frame in another space

Decoded and resized frames now report their source's space from `Surface::color`, and `Encoder::encode` fails with `Error::ColorMismatch` when a frame's space differs from the one the encoder signals, rather than mislabelling it.
Open the encoder with the first frame's space, and reopen it when the space changes.

```rust
// Before
let encoder = Encoder::new(&encode::Config::new(width, height, rate))?;
// After
let mut config = encode::Config::new(width, height, rate);
config.color = frame.surface.color();
let encoder = Encoder::new(&config)?;

### `encode::Producer` takes a catalog reservation (#5715)

`encode::Producer::new` and `with_track` take a `moq_mux::catalog::Reserved` instead of the `catalog::Producer`, so the caller declares the encoder's GOP as the timeline hint.

```rust
// Before
let producer = encode::Producer::new(broadcast, catalog, rendition)?;
// After
let producer = encode::Producer::new(broadcast, catalog.reserve().with_timeline_hint(gop), rendition)?;
```

### Capture takes one `encode::Capture`, and `Control::new` returns the driver

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
if let Err(Error::CutUnsupported(_)) = control.cut() {
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

```rust
// Before
producer.tick()?;
producer.observe_lag(lag)?;
producer.idle()?;
// After: delete the calls
```

### External Vulkan and DMA-BUF inputs (#4975)

On Linux, `Surface::Vulkan` no longer requires the `nvidia` feature.
`vulkan::Slot::new` takes exported handles, an `Image`, and a producer guard, replacing the CUDA-owned producer API.
`vulkan::Importer`, `vulkan::ImportError`, `cuda::Converter`, and `cuda::Slot` leave the public API; `Surface::Cuda` stays for NVDEC output.
`DmaBuf::new` takes an owned FD, a `DmaBufLayout`, and a release guard, and an external DMA-BUF refuses CPU readback.
Bind eager open to an external device with `encode::Config::input`; an unsupported device is refused instead of falling back to software.

```rust
// Before
let importer = frame::vulkan::Importer::new(ordinal, capacity)?;
let slot = importer.import(frame::vulkan::Handles::new(memory, timeline), image, owner)?;
// After
let slot = frame::vulkan::Slot::new(frame::vulkan::Handles { memory, timeline }, image, owner)?;
```

### Decoder staleness is `max_delay`

`decode::Options::max_age` is `max_delay`.

```rust
// Before
options.max_age = delay;
// After
options.max_delay = delay;
```
