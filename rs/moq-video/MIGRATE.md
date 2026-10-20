# Migrating moq-video

How to update your code for each breaking change, newest first.

## Unreleased

### OpenH264 is opt-in (#5771)

The default features drop `openh264`, so a default build has no software H.264 encoder or decoder and a GPU-less host fails to open one.
Add `features = ["openh264"]` to keep the fallback.

### `Color` is a struct of matrix, range, primaries and transfer

The four variants become constants plus a range, and the new `color` module holds `Matrix`, `Range`, `Primaries` and `Transfer`.
`Bt601Limited` is `Color::BT601`, `Bt709Limited` is `Color::BT709`, and the `Full` variants set `range: Range::Full`, as in `Color { range: Range::Full, ..Color::BT709 }`.
Match on the field you need instead of the variant, such as `color.matrix == Matrix::Bt709`.
BT.601 now names its primaries (SMPTE 170M), and BT.2020 with PQ or HLG is representable.

### `Encoder::encode` refuses a frame in another space

Decoded and resized frames now report their source's space from `Surface::color`, and `Encoder::encode` fails with `Error::ColorMismatch` when a frame's space differs from the one the encoder signals, rather than mislabelling it.
Set `encode::Config::color` to the first frame's `frame.surface.color()`, and reopen the encoder when the space changes.

### `encode::Producer` takes a catalog reservation (#5715)

`encode::Producer::new` and `with_track` take a `moq_mux::catalog::Reserved` instead of the `catalog::Producer`, so the caller declares the encoder's GOP as the timeline hint.
Pass `catalog.reserve().with_timeline_hint(gop)` instead of the catalog.

### Capture takes one `encode::Capture`, and `Control::new` returns the driver

`publish_capture(broadcast, catalog, capture, encode, clock)` is `publish_capture(broadcast, catalog, encode::Capture)`.
`Capture` has no clock: the driver stamps on `catalog.clock()`.
`encode::Control::new` returns the control handle and the driver; dropping the last `Control` cancels an in-flight open or encode.
Build it from `encode::Capture::default()`, setting `capture` and `encode`.

### `Control::cut` returns a `Result`

`cut()` fails with `Error::CutUnsupported` once the driver knows its backend cannot force a keyframe, and the publish carries on at its cadence.
It counts cadence keyframes too, so a request may be served without forcing one.
On `Error::CutUnsupported`, wait for the next cadence keyframe.

### `Encoded` marks keyframes (#4295)

`encode::Encoded::new` takes a third `keyframe: bool`, also readable as `Encoded::keyframe`.

### Producers drop `tick`, `idle`, and `observe_lag` (#4915)

`encode::Producer` no longer takes liveness hints, so delete the calls; disable a rendition through the catalog's `enabled` flag instead.

### External Vulkan and DMA-BUF inputs (#4975)

On Linux, `Surface::Vulkan` no longer requires the `nvidia` feature.
`vulkan::Slot::new` takes exported handles, an `Image`, and a producer guard, replacing the CUDA-owned producer API.
`vulkan::Importer`, `vulkan::ImportError`, `cuda::Converter`, and `cuda::Slot` leave the public API; `Surface::Cuda` stays for NVDEC output.
`DmaBuf::new` takes an owned FD, a `DmaBufLayout`, and a release guard, and an external DMA-BUF refuses CPU readback.
Bind eager open to an external device with `encode::Config::input`; an unsupported device is refused instead of falling back to software.
Replace `Importer::new` and `import` with `frame::vulkan::Slot::new(frame::vulkan::Handles { memory, timeline }, image, owner)`.

### Decoder staleness is `max_delay`

`decode::Options::max_age` is `max_delay`.
