# [L] Embedded video path

## Goal

`moq-video` presents on Raspberry Pi and similar embedded hardware. Encoding
and decoding there is the V4L2 M2M backends' job already; presenting is not
possible today, because the renderer's only zero-copy import is Vulkan and
those devices have no usable Vulkan driver.

## Plan

**EGL/GLES import** in the renderer. The devices with a V4L2 codec are the ones
without a usable Vulkan driver, so the existing DMA-BUF import cannot reach
them. Same shape as the Vulkan path: alias the buffer, keep the per-path
fallback and three-strike disable, fall back to I420 when import fails.

Hardware-gated end to end. It needs a real device to validate, and the usual
dlopen-and-degrade rule applies: a build without the device present must
degrade rather than fail to start.

Two items from the same wave are deliberately not here. X11 MIT-SHM capture is
optional, since portal and PipeWire cover modern desktops. A pre-encoded
libcamera source composes with what already exists, because
`encode::Producer::publish` is the bring-your-own-Annex-B path and
`moq_mux::codec::h264` already handles framing, so shelling out to
`rpicam-vid` is an application concern rather than a moq-video source.
