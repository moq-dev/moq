# [M] Studio on macOS and Windows

## Goal

`moq-studio` is run, not only compiled, on macOS and Windows, and whatever
only shows up there is fixed. Only Linux was built and run when the studio
landed.

## Plan

- **macOS:** camera, display, microphone, and system audio sources, plus the
  window, preview, program, and publish path.
- **Windows:** camera, display, window, and microphone sources, plus the
  window, preview, program, and publish path. System audio is `Unsupported`
  there until [Windows capture parity](/quest/m2/capture-windows.md) adds
  WASAPI loopback, so that quest runs it.
- Build with `--features capture`, since a default build has image, color,
  and broadcast sources only.

## Related

- [Zero-copy program encode](/quest/m1/studio-gpu/encode.md) - the GPU encode path, which lands without this run
- [Hardware validation](/quest/m3/video-hardware.md) - capture paths not yet run on real machines
