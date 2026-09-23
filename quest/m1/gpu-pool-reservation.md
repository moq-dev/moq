# [S] GPU frame pool back-pressure is a reservation, not an error

## Goal

A caller feeding imported Vulkan frames through `moq_video::frame::cuda::Converter`
can drop a frame when the bounded GPU pool is full without matching an error.
Exhaustion is expected back-pressure; only real failures are errors.

## Plan

`Converter::convert` and `cuda::Frame::resize` take a pooled buffer and report
a full pool as `Error::Unsupported` with a prose message (#3869), so the CARLA
bridge in moq.pro can only drop-and-continue by matching the text.

Split the reservation from the work, the way the bandwidth allocator hands out
a `Reservation`: `Converter::reserve() -> Option<Slot>` returns `None` when
every buffer is live, and `Slot::convert(&vulkan::Frame) -> Result<Frame, Error>`
does the GPU work on the held buffer, failing only for a genuine error. The
slot returns its buffer to the pool on drop, converted or not. Apply the same
shape to the resize pool. Keep the pool itself crate-private and its capacity
bound unchanged.

Tests on the injected allocator: `reserve` yields exactly `capacity` slots and
then `None`, dropping an unconverted slot frees it, and a failed conversion
does not leak the buffer. Update the `just rs vulkan-cuda` hardware test and
`doc/lib/rs/moq-video.md` inline.

Public API: `moq-video` 0.0.x, breaking for `Converter::convert` callers. Wire:
none.

## Related

- [Bandwidth allocator](/quest/m1/2848-follow-the-bandwidth-grant-in-moq-audio-instead-of.md) - the reservation-handle precedent
