# [M] Linux virtual camera

## Goal

The studio publishes a canvas's program as a PipeWire video node that
PipeWire-aware apps (browsers and conferencing apps) list as a camera. It
falls back to an existing v4l2loopback device when one is present.
