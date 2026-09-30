# [L] Windows.Graphics.Capture backend for display and window capture

## Goal

One Windows.Graphics.Capture (WGC) backend serves `Source::Display` and
`Source::Window` on Windows and honors `config.cursor` through
`IsCursorCaptureEnabled`. It replaces both current backends: DXGI Desktop
Duplication (`capture/desktopduplication.rs`, which ignores `config.cursor`)
and GDI window capture (`capture/window.rs`, whose `PrintWindow` runs on the
target's UI thread). Frames stay on the GPU as NV12 `Surface::Texture`.

Out of scope: application capture and system audio, which stay in
[Windows capture parity](/quest/m2/capture-windows.md). WGC captures one
monitor or one window per item and has no per-process source, so it does not
answer app capture.

## Plan

Decided in planning (09-30):

- **Replace, no fallback.** Delete `desktopduplication.rs` and the GDI capture
  path. Keeping DD as a fallback means two backends for Windows builds that
  are out of support anyway.
- **Mirror `capture/screencapture.rs`.** `CreateForMonitor` or
  `CreateForWindow` (through `IGraphicsCaptureItemInterop`) builds the
  `GraphicsCaptureItem`. Everything after that is shared: one session, a
  free-threaded frame pool, a first frame to learn the size, and a guard that
  closes the session on drop. `GraphicsCaptureItem.Closed` ends the stream.
- **GPU NV12 output.** Each BGRA pool frame goes through an
  `ID3D11VideoProcessor` blit into an owned NV12 `d3d11::Texture` on the
  capture device, so it can reach the Media Foundation hardware encoder
  zero-copy, just as camera frames do. Pool frames are recycled, so copying out
  is required. Generalize the existing NV12 `Scaler` in `frame.rs` to take BGRA
  input instead of writing a second processor. openh264 already downloads a
  `Texture` through `to_i420`.
- **The `windows` crate directly** (0.62, already a public re-export), not
  `windows-capture` or another wrapper. We need our own D3D11 device, so it can
  be shared with the encoder, and one `windows` version. This adds WinRT
  features (`Graphics_Capture`, `Graphics_DirectX_Direct3D11`,
  `Win32_System_WinRT_Graphics_Capture`, `Win32_System_WinRT_Direct3D11`,
  `Win32_Graphics_Dwm`, and whatever else they pull in). Adding features is
  additive.
- **Minimum Windows 10 2004 (build 19041).** That is where
  `IsCursorCaptureEnabled` appears. Refuse loud below it with
  `Error::Unsupported`, never silently drop the cursor. Set
  `IsBorderRequired = false` where the property exists (build 20348+); older
  builds keep the yellow capture border. Document the minimum in
  `doc/lib/rs/moq-video.md`.
- **Resize ends the stream.** When `ContentSize` changes, debounce with
  `Settle` and then return `Read::Done` so the caller reopens, the way
  `window.rs` does today. The encoder's geometry is fixed. This also removes
  DD's stale-size bug, where it keeps converting at the old width and height
  after a mode change.
- **Displays enumerate across every adapter.** Use `EnumDisplayMonitors`
  rather than adapter 0's DXGI outputs, which miss monitors attached to the
  other GPU on hybrid laptops. Keep the user-visible `display:N` ids and names.
- **Windows skip DWM-cloaked entries.** Filter on `DWMWA_CLOAKED` (hidden UWP
  windows and other virtual desktops), and size windows from `ContentSize`
  rather than `GetWindowRect`, which includes the invisible borders. Keep the
  `window:{hwnd}` ids.

Public API: no Rust signature changes. `config.cursor` starts working for
displays. Windows builds older than 19041 go from working to refused.

Verification: Windows CI only runs `cargo check`, and hosted runners have no
real desktop. Add `#[ignore]` display and window tests, run them by hand on
real Windows hardware, and record this checklist in the PR:

- cursor on and off
- window resize, close, and minimize
- multi-monitor
- one Windows 10 2004+ machine, showing that the yellow border appears there

Fix the docs this makes stale in the same PR: `rs/moq-video/README.md`,
`doc/lib/rs/moq-video.md`, `doc/bin/cli.md`, the `capture/mod.rs` module doc,
the Cargo feature comment, and the `frame.rs` docs on which producers make a
`Texture`.

## Related

- [Windows capture parity](/quest/m2/capture-windows.md) - app capture and system audio, which WGC does not answer
- [Capture frame buffers](/quest/m2/capture-frame-buffers.md) - its GDI half disappears with `window.rs`
- [Direct3D11 render import](/quest/m2/render-d3d11.md) - the other end of keeping Windows frames on the GPU
