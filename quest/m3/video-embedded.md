# [S] Embedded video path

## Goal

`moq-video` presents on a Raspberry Pi 4 or 5, verified on the device.
Encoding and decoding there is the V4L2 M2M backends' job already; this
checks whether the renderer's existing paths present those frames, and
records what breaks.

## Plan

Decided 2026-10-08: verify first. The Pi 4 and 5 ship Mesa's v3dv Vulkan
driver, so the premise that embedded devices have no usable Vulkan driver is
doubtful. Run the renderer's DMA-BUF Vulkan import against V4L2 M2M decoder
output, and the I420 CPU fallback, and record the device, OS image, Mesa
version, and which path presented.

If the Vulkan import cannot take the decoder's buffers (a missing extension
or modifier) on a device a consumer needs, plan EGL/GLES import as its own
quest: same shape as the Vulkan path (alias the buffer, keep the per-path
fallback and three-strike disable, fall back to I420 when import fails), and
the usual dlopen-and-degrade rule, so a build without the device present
degrades rather than fails to start.

Two items from the same wave are deliberately not here. X11 MIT-SHM capture is
optional, since portal and PipeWire cover modern desktops. A pre-encoded
libcamera source composes with what already exists, because
`encode::Producer::publish` is the bring-your-own-Annex-B path and
`moq_mux::codec::h264` already handles framing, so shelling out to
`rpicam-vid` is an application concern rather than a moq-video source.

## Required

- [Video validation hardware is on hand](/quest/m3/video-hardware-access.md) - the Pi to validate on
