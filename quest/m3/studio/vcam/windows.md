# [M] Windows virtual camera

## Goal

On Windows 11, the studio registers a canvas's program as a session-lifetime
camera through `MFCreateVirtualCamera`, backed by a software camera source.
Apps list it as a camera while the studio runs.

## Required

- [Studio program encode from the GPU](/quest/m3/studio/app.md) - the studio running on Windows
