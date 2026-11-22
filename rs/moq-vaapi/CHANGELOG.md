# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- `Image` no longer hands out `&mut [u8]` from a shared `Surface` borrow, which let safe code alias one derived surface's pixels mutably. Writing goes through the new `ImageMut`, which borrows the surface mutably, unmaps its buffer before `vaPutImage`, and reports a failed `put` instead of dropping it.
- `Encoder::encode_nv12` no longer reads the input surface back (`vaGetImage`) or queries the image formats on every frame, and no longer panics on a plane whose last row is shorter than its pitch. A 1080p frame encodes about 1 ms faster on an RX 9070 and an Arrow Lake iGPU.
- A driver returning an invalid image mapping, a null coded segment, a negative fd, more export objects or list entries than fit, or a layer naming a missing object is an error instead of a panic or undefined behavior, and surface attribute queries no longer write through a shared reference.
- The decoders fail a picture whose order count overflows `i32` instead of panicking in debug builds, and an H.264 frame_num gap invents at most `max_num_ref_frames` pictures instead of up to MaxFrameNum per slice.
- The H.264 decoder releases a picture as soon as the SPS's reorder limit allows, as the H.265 decoder already did, instead of holding it until a later picture needs its frame buffer. A stream without reordering, such as openh264's, comes out as it decodes rather than one access unit late. A stream that reorders more than its VUI declares raises the limit for itself, up to the DPB size.

### Changed

- **Breaking:** `Image::derive_from` and `Image::create_from` return read-only images (`AsRef` only); write through `ImageMut::create_uninitialized(&mut surface, format)` and finish with `put`.
- **Breaking:** `encode::Config::gop` is a `NonZeroU32`, and `Encoder::new` refuses an odd or empty frame size.
- Moved into [moq-dev/moq](https://github.com/moq-dev/moq) from [moq-dev/vaapi](https://github.com/moq-dev/vaapi), whose history covers 0.2.1 and earlier.
- Logs through `tracing` instead of `log`.
