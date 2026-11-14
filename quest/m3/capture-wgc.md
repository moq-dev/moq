# [S] Verify Windows.Graphics.Capture on real hardware

## Goal

The WGC backend for `Source::Display` and `Source::Window` (`capture/wgc.rs`,
`capture/wgc/native.rs`) behaves correctly on real Windows desktops. Hosted
Windows CI has no desktop, so only the notification, pacing, startup, and
selector logic run there. Fix any bug the matrix turns up.

Out of scope: application capture and system audio, which stay in
[Windows capture parity](/quest/m3/capture-windows.md).

## Plan

Run the ignored `wgc_*` tests and `moq` capture by hand, and record each
result in the PR:

- cursor on and off
- an odd-sized window
- color on an SD and an HD source, from BGRA through NV12 to the encoder, on
  both the Media Foundation path and the openh264 readback path
- window resize, close, and minimize, including minimizing between
  `StartCapture` and the first frame
- a static screen keeps delivering paced frames without the rendition stalling
- multi-monitor, including a hybrid-GPU laptop
- one Windows 10 2004+ machine, showing that the yellow border appears there

Full-display capture with the cursor on was verified on Windows 11 25H2 with
one monitor.

Moved to m3 2026-11-12 (maintainer, /quest-audit): a downstream application
now runs display and window capture on Windows.

Note: the Windows host on hand has one monitor and one discrete GPU. Run
every item it can; the multi-monitor, hybrid-GPU laptop, and Windows 10
items need hardware nobody has, so record them as not run and leave them to
[video hardware validation](/quest/m5/video-hardware.md) rather than holding
this quest open for them.

## Related

- [Windows capture parity](/quest/m3/capture-windows.md) - app capture and system audio, which WGC does not answer
- [Direct3D11 render import](/quest/m4/render-d3d11.md) - the other end of keeping Windows frames on the GPU
