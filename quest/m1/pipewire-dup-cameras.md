# [XS] A webcam lists once with PipeWire enabled

## Goal

`moq_video::capture::cameras()` on Linux with the `pipewire` feature lists a
webcam once. Today a UVC webcam appears as its V4L2 device and again as the
PipeWire node that wraps it ([#4022](https://github.com/moq-dev/moq/pull/4022)).

## Plan

Decided: hide PipeWire camera nodes with `device.api = v4l2` whose device the
V4L2 backend already listed, keeping the shorter list over exposing the
backend choice. Nodes V4L2 cannot see stay: libcamera cameras (a Raspberry Pi
CSI camera) and everything inside a sandbox, where V4L2 lists nothing.

Guidance:

- Match on the node's V4L2 device path property (`api.v4l2.path`), not the
  description, so two identical webcams stay distinct.
- Only the listing changes. `pipewire:<name>` still opens a hidden node, and
  the `pipewire` default keeps resolving by priority.
- Update the `cameras()` doc that currently says a webcam appears once per
  backend, and cover the filter with a unit test over scanned node
  properties.
