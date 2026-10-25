# [M] Studio on macOS and Windows

## Goal

`moq-studio` is run, not only compiled, on macOS and Windows, and whatever
only shows up there is fixed. Only Linux was built and run when the studio
landed.

## Plan

- **macOS:** camera, display, microphone, and system audio sources, plus the
  window, preview, program, and publish path.
- **Windows:** camera, display, window, microphone, and system audio sources,
  plus the window, preview, program, and publish path.
- Build with `--features capture`, since a default build has image, color,
  and broadcast sources only.

## Related

- [Zero-copy program encode](/quest/m2/studio-gpu/encode.md) - the GPU encode path, which lands without this run
- [Hardware validation](/quest/m3/video-hardware.md) - capture paths not yet run on real machines
