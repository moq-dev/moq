# [L] Embedded video path

## Goal

`moq-video` encodes and decodes on Raspberry Pi and similar embedded hardware,
and can present there. Neither is possible today: there is no stateful V4L2
codec backend, and the renderer's only zero-copy import is Vulkan.

## Plan

**V4L2 M2M encode and decode.** The stateful hardware codec path embedded
devices expose, and the one gap with no alternative: openh264 software encode
on a Pi is not real-time. The seam these need already exists, since timestamps
ride on `Frame` through encode, so a queued device draining frame N-k while
frame N is submitted stamps its output correctly rather than with the
submission clock.

**EGL/GLES import** in the renderer. The same devices are the ones without a
usable Vulkan driver, so the existing DMA-BUF import cannot reach them. Same
shape as the Vulkan path: alias the buffer, keep the per-path fallback and
three-strike disable, fall back to I420 when import fails.

Hardware-gated end to end. Both halves need a real device to validate, and the
usual dlopen-and-degrade rule applies: a build without the device present must
degrade rather than fail to start.

Two items from the same wave are deliberately not here. X11 MIT-SHM capture is
optional, since portal and PipeWire cover modern desktops. A pre-encoded
libcamera source composes with what already exists, because
`encode::Producer::publish` is the bring-your-own-Annex-B path and
`moq_mux::codec::h264` already handles framing, so shelling out to
`rpicam-vid` is an application concern rather than a moq-video source.
