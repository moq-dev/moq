# Migrating moq-video

How to update your code for each breaking change, newest first.

## Unreleased

### A capture reopened onto a new identity publishes a new track ([#7110](https://github.com/moq-dev/moq/pull/7110))

When a reopened camera or encoder emits a different codec string than the probed rendition, such as a higher level for a larger mode, `encode::Producer` publishes under the next free `<name>/<n>` and finishes the old track.
`Producer::demand` follows the current track, so read it again after `publish` rather than holding the first one.

### The renderer refuses GPU frames it cannot import (#6806)

`render::Renderer::render` no longer downloads a GPU surface it cannot import; it returns an error naming the cause and the fix.
`render::Config::zero_copy` is gone, along with the retry-and-retire fallback behind it.
CPU frames (`Surface::I420`) upload as before.
Decode and capture with `render::output(&device)` (`decode::Config::output`, `capture::Config::output`) to receive only frames the renderer on that device draws.
It picks `Output::Render`, which keeps the surfaces this platform imports on the GPU and downloads the rest, for a device that can import, and `Output::Cpu` for one that cannot (wgpu's GL backend, or a Linux device without DMA-BUF import).
Either errors on a surface with no download, such as a non-NV12 pixel buffer.
On Linux, open the device with `wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF` where the adapter has it, or `render::output` picks the CPU.
To force the CPU path that `zero_copy: false` gave, decode or capture with `Output::Cpu`.

### `Rate` is `FrameRate` (#6653)

`Rate` and `RateError` are `FrameRate` and `FrameRateError`, the name the bindings already use.
Rename the imports; the methods and error variants are unchanged.

### Decoded reads carry gaps and encoder ends (#6475)

`decode::Consumer::read()` returns `decode::Read` events instead of bare frames, the same shape as `moq_audio::decode::Read`: `Frame(frame)`, `Gap { timestamp, duration }` for lost media, and `End { end }` where the publisher stopped its encoder; `None` still means the track is finished.
Match `decode::Read::Frame(frame)` where you took the frame before, and skip the other variants to keep the old behavior.
An `End` now drains the decoder, so the last picture before a pause arrives without waiting for the next one.

### `Source::Display` no longer opens the portal on Wayland

`capture::Source::Display` captures X11 displays only, including XWayland's `x11:` ids.
On Wayland the main display and any non-`x11:` id now fail with `Error::Unsupported` instead of opening the system picker with a selection shared by every publication in the process.
Pass `Source::Portal(portal::Selection::new(portal::Kind::Screen))` to ask the picker, and keep that selection for the publication's reopens.

### Native frame imports use platform and format roles

`frame::macos` is `frame::apple`.
`frame::vulkan::Channels` is `Format`. `Image` exposes `device`, `memory`, `size`, `allocation_size`, and `format` as fields; replace `device_uuid()` with `device.device_uuid`, `size()` with `size`, `allocation_size()` with `allocation_size`, and `channels()` with `format`.
Construct `Handles { memory, timeline }` directly instead of calling `Handles::new`.

### Capture advertises refresh (#6048)

Capture tracks now advertise `refresh: Some(2)` when every backend the selection may open honors a cut, and the driver forces a keyframe when `Demand::wanted` passes the largest group.

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

`encode::Producer::new` and `with_track` take a `moq_mux::catalog::Reserved` instead of the `catalog::Producer`, so the caller declares the encoder's GOP as the HLS hint.
Pass `catalog.reserve().with_hls_hint(gop)` instead of the catalog.

### Capture takes one `encode::Capture`

`publish_capture(broadcast, catalog, capture, encode, clock)` is `publish_capture(broadcast, catalog, encode::Capture)`.
`Capture` has no clock: the driver stamps on `catalog.clock()`.
Build it from `encode::Capture::default()`, setting `capture` and `encode`.

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
